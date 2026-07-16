use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, atomic::AtomicU64},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use sigil_kernel::{
    EXECUTION_OUTPUT_RECEIPT_SCHEMA_VERSION, ExecutionBackend, ExecutionBackendCapabilities,
    ExecutionBackendKind, ExecutionBackendSelectionDiagnostic, ExecutionCleanupReceipt,
    ExecutionConfig, ExecutionNetworkReceipt, ExecutionOutputReceipt, ExecutionReceipt,
    ExecutionRequest, ExecutionResourceLimitKind, ExecutionResourceLimitReceipt,
    ExecutionResourceReceipt, ExecutionSandboxFallback, ExecutionSandboxProfile,
    ExecutionTerminationCause, ExecutionTimeoutSource, ProcessEnvironmentPolicy,
    ResolvedProcessEnvironment, validate_extension_process_isolation,
};
use tokio::{
    process::{Child, Command},
    sync::mpsc,
    task::JoinHandle,
    time::Instant as TokioInstant,
};

#[cfg(unix)]
use crate::process_group::{
    process_group_has_live_members as process_group_is_alive,
    send_process_group_signal as send_signal_to_process_group,
};

mod bubblewrap;
mod docker;
mod local;
mod output;
mod seatbelt;

pub use bubblewrap::LinuxBubblewrapExecutionBackend;
pub use docker::DockerExecutionBackend;
pub use local::LocalExecutionBackend;
pub use seatbelt::MacosSeatbeltExecutionBackend;

pub(crate) use bubblewrap::ensure_linux_bubblewrap_available;
pub(crate) use bubblewrap::linux_bubblewrap_args;
pub(crate) use docker::ensure_docker_available;
pub(crate) use seatbelt::ensure_macos_seatbelt_available;
pub(crate) use seatbelt::macos_seatbelt_workspace_write_profile;

use output::{CollectedPipe, OutputAlert, OutputCollectionLimits, collect_async_pipe};

const OUTPUT_DRAIN_GRACE: Duration = Duration::from_secs(1);
const PROCESS_TERM_GRACE: Duration = Duration::from_millis(500);
const PROCESS_KILL_VERIFY_GRACE: Duration = Duration::from_secs(3);
const PROCESS_CLEANUP_COMMAND_TIMEOUT: Duration = Duration::from_secs(1);

#[cfg(test)]
pub(crate) use docker::current_user_group_flag;

pub(crate) fn configure_command_environment(command: &mut Command, request: &ExecutionRequest) {
    if request.environment_policy.clears_parent() {
        command.env_clear();
    }
    command.envs(&request.env);
}

/// Command plan for a long-lived stdio process that needs the same sandbox policy as built-in tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LongLivedStdioProcessPlan {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub environment: ResolvedProcessEnvironment,
    pub backend: ExecutionBackendKind,
    pub backend_capabilities: ExecutionBackendCapabilities,
    pub sandbox_profile: ExecutionSandboxProfile,
    pub network: ExecutionNetworkReceipt,
    pub sandboxed: bool,
}

/// Builds a command plan for long-lived stdio processes such as local MCP servers.
///
/// # Errors
///
/// Returns an error when the configured backend cannot preserve long-lived stdio pipes while
/// satisfying the requested sandbox/profile policy.
pub fn long_lived_stdio_process_plan(
    config: &ExecutionConfig,
    program: &str,
    args: &[String],
    cwd: &Path,
    environment: &ResolvedProcessEnvironment,
) -> Result<LongLivedStdioProcessPlan> {
    if program.trim().is_empty() {
        bail!("long-lived stdio process command must not be empty");
    }
    let canonical_cwd = fs::canonicalize(cwd)
        .with_context(|| format!("failed to canonicalize cwd {}", cwd.display()))?;
    match config.backend() {
        ExecutionBackendKind::Local => {
            let backend = LocalExecutionBackend;
            validate_extension_process_isolation(
                config.profile(),
                backend.capabilities(),
                &backend.planned_network_receipt(),
                "mcp_stdio",
            )?;
            if config.requires_sandbox() {
                bail!(
                    "MCP stdio sandbox unavailable: local execution backend cannot enforce local stdio sandbox"
                );
            }
            Ok(LongLivedStdioProcessPlan {
                program: PathBuf::from(program),
                args: args.iter().map(OsString::from).collect(),
                cwd: canonical_cwd,
                environment: environment.clone(),
                backend: ExecutionBackendKind::Local,
                backend_capabilities: ExecutionBackendCapabilities::default(),
                sandbox_profile: ExecutionSandboxProfile::Unconfined,
                network: ExecutionNetworkReceipt::unknown(
                    "local MCP stdio process does not report network enforcement",
                ),
                sandboxed: false,
            })
        }
        ExecutionBackendKind::MacosSeatbelt => {
            let backend = MacosSeatbeltExecutionBackend::default()
                .with_network_allowed(config.profile_spec().network_allowed);
            let capabilities = backend.capabilities();
            validate_long_lived_stdio_capabilities(
                config,
                capabilities,
                &backend.planned_network_receipt(),
            )?;
            if let Err(error) = ensure_macos_seatbelt_available(&backend) {
                bail!("MCP stdio sandbox unavailable: {error}");
            }
            let profile = macos_seatbelt_workspace_write_profile(&canonical_cwd);
            let mut planned_args = vec![
                OsString::from("-p"),
                OsString::from(profile),
                OsString::from(program),
            ];
            planned_args.extend(args.iter().map(OsString::from));
            Ok(LongLivedStdioProcessPlan {
                program: PathBuf::from("/usr/bin/sandbox-exec"),
                args: planned_args,
                cwd: canonical_cwd,
                environment: environment.clone(),
                backend: ExecutionBackendKind::MacosSeatbelt,
                backend_capabilities: capabilities,
                sandbox_profile: config.profile(),
                network: if config.profile_spec().network_allowed {
                    ExecutionNetworkReceipt::allowed("profile allows network access")
                } else {
                    ExecutionNetworkReceipt::unsupported(
                        "macos_seatbelt cannot prove network denial",
                    )
                },
                sandboxed: true,
            })
        }
        ExecutionBackendKind::LinuxBubblewrap => {
            let Some(bwrap) = find_executable_on_path("bwrap") else {
                bail!(
                    "MCP stdio sandbox unavailable: linux_bubblewrap execution backend requires bwrap on PATH"
                );
            };
            let backend = LinuxBubblewrapExecutionBackend::new(
                bwrap.clone(),
                config.profile_spec().network_allowed,
            );
            let capabilities = backend.capabilities();
            validate_long_lived_stdio_capabilities(
                config,
                capabilities,
                &backend.planned_network_receipt(),
            )?;
            if let Err(error) = ensure_linux_bubblewrap_available(&backend) {
                bail!("MCP stdio sandbox unavailable: {error}");
            }
            let request = ExecutionRequest {
                program: program.to_owned(),
                args: args.to_vec(),
                cwd: canonical_cwd.clone(),
                env: BTreeMap::new(),
                environment_policy: ProcessEnvironmentPolicy::IsolatedExtension,
                timeout_ms: None,
                timeout_secs: 0,
                cpu_time_ms: None,
                memory_limit_bytes: None,
                process_count_limit: None,
            };
            let mut planned_args = linux_bubblewrap_args(
                &canonical_cwd,
                &request,
                config.profile_spec().network_allowed,
            );
            planned_args.push(OsString::from(program));
            planned_args.extend(args.iter().map(OsString::from));
            Ok(LongLivedStdioProcessPlan {
                program: bwrap,
                args: planned_args,
                cwd: canonical_cwd,
                environment: environment.clone(),
                backend: ExecutionBackendKind::LinuxBubblewrap,
                backend_capabilities: capabilities,
                sandbox_profile: config.profile(),
                network: if config.profile_spec().network_allowed {
                    ExecutionNetworkReceipt::allowed("profile allows network access")
                } else {
                    ExecutionNetworkReceipt::denied("bubblewrap uses --unshare-net")
                },
                sandboxed: true,
            })
        }
        ExecutionBackendKind::Docker => {
            bail!(
                "MCP stdio sandbox unavailable: docker execution backend does not support long-lived stdio MCP processes"
            )
        }
    }
}

fn validate_long_lived_stdio_capabilities(
    config: &ExecutionConfig,
    capabilities: ExecutionBackendCapabilities,
    planned_network: &ExecutionNetworkReceipt,
) -> Result<()> {
    validate_extension_process_isolation(
        config.profile(),
        capabilities,
        planned_network,
        "mcp_stdio",
    )?;
    let requirements = config.required_capabilities_for_persistent_pty();
    let missing = capabilities.missing_requirements(requirements);
    if !missing.is_empty() {
        bail!(
            "MCP stdio sandbox unavailable: execution backend {} missing capabilities: {}",
            config.backend().as_str(),
            missing
                .iter()
                .map(|capability| capability.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if let Err(error) = config.validate_profile_capabilities(capabilities) {
        bail!("MCP stdio sandbox unavailable: {error}");
    }
    Ok(())
}

/// Builds the configured execution backend for built-in tools.
///
/// # Errors
///
/// Returns an error when configuration requires sandbox enforcement that the selected backend
/// cannot provide.
pub fn build_execution_backend(config: &ExecutionConfig) -> Result<Arc<dyn ExecutionBackend>> {
    let backend: Arc<dyn ExecutionBackend> = match config.backend() {
        ExecutionBackendKind::Local => Arc::new(LocalExecutionBackend),
        ExecutionBackendKind::MacosSeatbelt => {
            let backend = MacosSeatbeltExecutionBackend::default()
                .with_network_allowed(config.profile_spec().network_allowed);
            if let Err(error) = ensure_macos_seatbelt_available(&backend) {
                return fallback_or_error(
                    config,
                    ExecutionBackendSelectionDiagnostic::unavailable(config, error.to_string()),
                );
            }
            Arc::new(backend)
        }
        ExecutionBackendKind::LinuxBubblewrap => {
            let Some(bwrap) = find_executable_on_path("bwrap") else {
                return fallback_or_error(
                    config,
                    ExecutionBackendSelectionDiagnostic::unavailable(
                        config,
                        "linux_bubblewrap execution backend requires bwrap on PATH",
                    ),
                );
            };
            let backend =
                LinuxBubblewrapExecutionBackend::new(bwrap, config.profile_spec().network_allowed);
            if let Err(error) = ensure_linux_bubblewrap_available(&backend) {
                return fallback_or_error(
                    config,
                    ExecutionBackendSelectionDiagnostic::unavailable(config, error.to_string()),
                );
            }
            Arc::new(backend)
        }
        ExecutionBackendKind::Docker => {
            let Some(image) = config.container_image() else {
                return fallback_or_error(
                    config,
                    ExecutionBackendSelectionDiagnostic::unavailable(
                        config,
                        "docker execution backend requires execution.sandbox.container_image",
                    ),
                );
            };
            let Some(docker) = find_executable_on_path("docker") else {
                return fallback_or_error(
                    config,
                    ExecutionBackendSelectionDiagnostic::unavailable(
                        config,
                        "docker execution backend requires docker on PATH",
                    ),
                );
            };
            let backend = DockerExecutionBackend::new(
                docker,
                image.to_owned(),
                config.profile_spec().network_allowed,
            );
            ensure_docker_available(&backend)?;
            Arc::new(backend)
        }
    };
    if let Err(diagnostic) = validate_execution_backend(config, backend.as_ref()) {
        return fallback_or_error(config, diagnostic);
    }
    Ok(backend)
}

pub(crate) fn validate_execution_backend(
    config: &ExecutionConfig,
    backend: &dyn ExecutionBackend,
) -> std::result::Result<(), ExecutionBackendSelectionDiagnostic> {
    if config
        .validate_profile_capabilities(backend.capabilities())
        .is_err()
    {
        return Err(ExecutionBackendSelectionDiagnostic::missing_capabilities(
            config,
            backend.capabilities(),
        ));
    }
    Ok(())
}

pub(crate) fn fallback_or_error(
    config: &ExecutionConfig,
    diagnostic: ExecutionBackendSelectionDiagnostic,
) -> Result<Arc<dyn ExecutionBackend>> {
    match config.fallback() {
        ExecutionSandboxFallback::Unconfined => Ok(Arc::new(LocalExecutionBackend)),
        ExecutionSandboxFallback::Deny | ExecutionSandboxFallback::Prompt => {
            bail!("{}", execution_backend_selection_error(config, &diagnostic))
        }
    }
}

pub(crate) fn execution_backend_selection_error(
    config: &ExecutionConfig,
    diagnostic: &ExecutionBackendSelectionDiagnostic,
) -> String {
    let missing = diagnostic.missing_capability_labels();
    let missing = if missing.is_empty() {
        "none".to_owned()
    } else {
        missing.join(", ")
    };
    let availability = diagnostic
        .availability_reason
        .as_deref()
        .unwrap_or("available");
    let fallback = match diagnostic.fallback {
        ExecutionSandboxFallback::Deny => "fallback denied",
        ExecutionSandboxFallback::Prompt => "fallback requires user prompt",
        ExecutionSandboxFallback::Unconfined => "fallback unconfined",
    };
    if let Err(reason) =
        config.validate_profile_capabilities(diagnostic.capabilities.unwrap_or_default())
    {
        return format!(
            "execution backend selection failed: requested_backend={}, requested_profile={:?}, missing_capabilities={missing}, availability={availability}, {fallback}: {reason}",
            diagnostic.requested_backend.as_str(),
            diagnostic.requested_profile,
        );
    }
    format!(
        "execution backend selection failed: requested_backend={}, requested_profile={:?}, missing_capabilities={missing}, availability={availability}, {fallback}",
        diagnostic.requested_backend.as_str(),
        diagnostic.requested_profile,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreflightReaderFault {
    None,
    #[cfg(test)]
    PanicStdout,
}

#[derive(Debug)]
pub(crate) struct PreflightCommandStatus {
    success: bool,
}

impl PreflightCommandStatus {
    pub(crate) fn success(&self) -> bool {
        self.success
    }
}

impl std::fmt::Display for PreflightCommandStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(if self.success { "success" } else { "failure" })
    }
}

#[derive(Debug)]
pub(crate) struct PreflightCommandOutput {
    pub(crate) status: PreflightCommandStatus,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

pub(crate) fn command_output_with_timeout(
    program: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<PreflightCommandOutput> {
    command_output_with_timeout_inner(program, args, timeout, PreflightReaderFault::None)
}

#[cfg(test)]
pub(crate) fn command_output_with_timeout_with_reader_panic(
    program: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<PreflightCommandOutput> {
    command_output_with_timeout_inner(program, args, timeout, PreflightReaderFault::PanicStdout)
}

fn command_output_with_timeout_inner(
    program: &Path,
    args: &[&str],
    timeout: Duration,
    _reader_fault: PreflightReaderFault,
) -> Result<PreflightCommandOutput> {
    let program = program.to_path_buf();
    let args = args.iter().map(OsString::from).collect::<Vec<_>>();
    let display_args = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let supervisor = thread::Builder::new()
        .name("sigil-preflight-supervisor".to_owned())
        .spawn(move || -> Result<ExecutionReceipt> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("failed to build preflight supervisor runtime")?;
            runtime.block_on(bounded_preflight_command_output(
                program,
                args,
                timeout,
                _reader_fault,
            ))
        })
        .context("failed to spawn preflight supervisor thread")?;
    let receipt = supervisor
        .join()
        .map_err(|_| anyhow::anyhow!("preflight supervisor thread panicked"))??;
    let output = receipt.effective_output();
    if !matches!(output.termination, ExecutionTerminationCause::Exited) {
        let stderr = String::from_utf8_lossy(&receipt.stderr);
        bail!(
            "bounded preflight {} {} failed: {}{}",
            receipt.backend.as_str(),
            display_args.join(" "),
            output.termination.as_str(),
            if stderr.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", stderr.trim())
            }
        );
    }
    Ok(PreflightCommandOutput {
        status: PreflightCommandStatus {
            success: receipt.exit_code == Some(0),
        },
        stdout: receipt.stdout,
        stderr: receipt.stderr,
    })
}

async fn bounded_preflight_command_output(
    program: PathBuf,
    args: Vec<OsString>,
    timeout: Duration,
    reader_fault: PreflightReaderFault,
) -> Result<ExecutionReceipt> {
    let request = ExecutionRequest {
        program: program.to_string_lossy().into_owned(),
        args: args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect(),
        cwd: std::env::current_dir().context("failed to resolve current directory")?,
        env: BTreeMap::new(),
        environment_policy: ProcessEnvironmentPolicy::InheritParent,
        timeout_ms: Some(timeout.as_millis().min(u128::from(u64::MAX)) as u64),
        timeout_secs: 0,
        cpu_time_ms: None,
        memory_limit_bytes: None,
        process_count_limit: None,
    };
    let mut command = Command::new(program);
    command.args(args).kill_on_drop(true);
    command_output_to_receipt_with_limits(
        ExecutionBackendKind::Local,
        ExecutionBackendCapabilities::default(),
        ExecutionNetworkReceipt::unknown("bounded local preflight command"),
        command,
        &request,
        OutputCollectionLimits::preflight(),
        reader_fault,
        None,
        None,
    )
    .await
}

pub(crate) fn find_executable_on_path(program: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

pub(crate) async fn bounded_short_command_output(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<ExecutionReceipt> {
    let args = args.iter().map(OsString::from).collect::<Vec<_>>();
    bounded_short_path_command_output(Path::new(program), &args, timeout).await
}

pub(super) async fn bounded_short_path_command_output(
    program: &Path,
    args: &[OsString],
    timeout: Duration,
) -> Result<ExecutionReceipt> {
    bounded_short_path_command_output_with_environment(
        program,
        args,
        timeout,
        ProcessEnvironmentPolicy::InheritParent,
        &BTreeMap::new(),
    )
    .await
}

pub(super) async fn bounded_short_path_command_output_with_environment(
    program: &Path,
    args: &[OsString],
    timeout: Duration,
    environment_policy: ProcessEnvironmentPolicy,
    env: &BTreeMap<String, String>,
) -> Result<ExecutionReceipt> {
    let request = ExecutionRequest {
        program: program.to_string_lossy().into_owned(),
        args: args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect(),
        cwd: std::env::current_dir().context("failed to resolve current directory")?,
        env: env.clone(),
        environment_policy,
        timeout_ms: Some(timeout.as_millis().min(u128::from(u64::MAX)) as u64),
        timeout_secs: 0,
        cpu_time_ms: None,
        memory_limit_bytes: None,
        process_count_limit: None,
    };
    let mut command = Command::new(program);
    command.args(args).kill_on_drop(true);
    configure_command_environment(&mut command, &request);
    command_output_to_receipt_with_limits(
        ExecutionBackendKind::Local,
        ExecutionBackendCapabilities::default(),
        ExecutionNetworkReceipt::unknown("bounded local preflight command"),
        command,
        &request,
        OutputCollectionLimits::preflight(),
        PreflightReaderFault::None,
        None,
        None,
    )
    .await
}

pub(crate) async fn command_output_to_receipt_with_cancellation(
    backend: ExecutionBackendKind,
    capabilities: ExecutionBackendCapabilities,
    network: ExecutionNetworkReceipt,
    command: Command,
    request: &ExecutionRequest,
    cancellation: Option<sigil_kernel::RunCancellationHandle>,
) -> Result<ExecutionReceipt> {
    command_output_to_receipt_with_limits(
        backend,
        capabilities,
        network,
        command,
        request,
        OutputCollectionLimits::execution(),
        PreflightReaderFault::None,
        None,
        cancellation,
    )
    .await
}

async fn command_output_to_receipt_with_docker_cleanup(
    backend: ExecutionBackendKind,
    capabilities: ExecutionBackendCapabilities,
    network: ExecutionNetworkReceipt,
    command: Command,
    request: &ExecutionRequest,
    docker_cleanup: docker::DockerContainerCleanup,
    cancellation: Option<sigil_kernel::RunCancellationHandle>,
) -> Result<ExecutionReceipt> {
    command_output_to_receipt_with_limits(
        backend,
        capabilities,
        network,
        command,
        request,
        OutputCollectionLimits::execution(),
        PreflightReaderFault::None,
        Some(docker_cleanup),
        cancellation,
    )
    .await
}

async fn command_output_to_receipt_with_limits(
    backend: ExecutionBackendKind,
    capabilities: ExecutionBackendCapabilities,
    network: ExecutionNetworkReceipt,
    mut command: Command,
    request: &ExecutionRequest,
    output_limits: OutputCollectionLimits,
    reader_fault: PreflightReaderFault,
    docker_cleanup: Option<docker::DockerContainerCleanup>,
    cancellation: Option<sigil_kernel::RunCancellationHandle>,
) -> Result<ExecutionReceipt> {
    #[cfg(not(test))]
    let _ = reader_fault;
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.kill_on_drop(true);
    configure_execution_process_group(&mut command);

    let deadline = request
        .timeout_duration()
        .map(|timeout| {
            TokioInstant::now().checked_add(timeout).ok_or_else(|| {
                anyhow::anyhow!("execution timeout exceeds the supported monotonic deadline")
            })
        })
        .transpose()?;

    let mut child = command.spawn()?;
    let process_id = child.id();
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let cleanup =
                cleanup_execution_child(&mut child, process_id, docker_cleanup.as_ref()).await;
            bail!("execution stdout pipe is unavailable after spawn; cleanup={cleanup:?}");
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            drop(stdout);
            let cleanup =
                cleanup_execution_child(&mut child, process_id, docker_cleanup.as_ref()).await;
            bail!("execution stderr pipe is unavailable after spawn; cleanup={cleanup:?}");
        }
    };
    let combined_total = Arc::new(AtomicU64::new(0));
    let stdout_total = Arc::new(AtomicU64::new(0));
    let stderr_total = Arc::new(AtomicU64::new(0));
    let (alert_tx, mut alert_rx) = mpsc::channel(8);
    let stdout_task = Some({
        let stdout_alert_tx = alert_tx.clone();
        let stdout_stream_total = Arc::clone(&stdout_total);
        let stdout_total = Arc::clone(&combined_total);
        tokio::spawn(async move {
            #[cfg(test)]
            let stdout = {
                let mut stdout = stdout;
                if reader_fault == PreflightReaderFault::PanicStdout {
                    let mut ready = [0_u8; 1];
                    let _ = tokio::io::AsyncReadExt::read_exact(&mut stdout, &mut ready).await;
                    panic!("injected preflight stdout reader panic");
                }
                stdout
            };
            collect_async_pipe(
                stdout,
                sigil_kernel::ExecutionOutputStream::Stdout,
                output_limits,
                stdout_stream_total,
                stdout_total,
                stdout_alert_tx,
            )
            .await
        })
    });
    let stderr_task = Some({
        tokio::spawn(collect_async_pipe(
            stderr,
            sigil_kernel::ExecutionOutputStream::Stderr,
            output_limits,
            Arc::clone(&stderr_total),
            Arc::clone(&combined_total),
            alert_tx,
        ))
    });

    let mut stdout_task = PipeTaskState::new(
        sigil_kernel::ExecutionOutputStream::Stdout,
        stdout_task,
        output_limits,
        Arc::clone(&stdout_total),
    );
    let mut stderr_task = PipeTaskState::new(
        sigil_kernel::ExecutionOutputStream::Stderr,
        stderr_task,
        output_limits,
        Arc::clone(&stderr_total),
    );
    let supervisor_event = wait_for_child_or_output_alert(
        &mut child,
        &mut alert_rx,
        deadline,
        &mut stdout_task,
        &mut stderr_task,
        cancellation.as_ref(),
    )
    .await;
    let mut exit_code = None;
    let mut termination = ExecutionTerminationCause::Exited;
    let mut cleanup = ExecutionCleanupReceipt::not_needed();
    match supervisor_event {
        SupervisorEvent::Child(status) => match status {
            Ok(status) => {
                exit_code = status.code();
                if let Ok(alert) = alert_rx.try_recv() {
                    termination = alert.termination();
                    cleanup =
                        cleanup_execution_child(&mut child, process_id, docker_cleanup.as_ref())
                            .await;
                }
            }
            Err(error) => {
                termination = ExecutionTerminationCause::ReaderFailed {
                    stream: sigil_kernel::ExecutionOutputStream::Combined,
                    reason: format!("failed to wait for execution child: {error}"),
                };
                cleanup =
                    cleanup_execution_child(&mut child, process_id, docker_cleanup.as_ref()).await;
            }
        },
        SupervisorEvent::Alert(alert) => {
            termination = alert.termination();
            cleanup =
                cleanup_execution_child(&mut child, process_id, docker_cleanup.as_ref()).await;
        }
        SupervisorEvent::Timeout => {
            termination = ExecutionTerminationCause::TimedOut;
            cleanup =
                cleanup_execution_child(&mut child, process_id, docker_cleanup.as_ref()).await;
        }
        SupervisorEvent::Cancelled => {
            termination = ExecutionTerminationCause::Cancelled;
            cleanup =
                cleanup_execution_child(&mut child, process_id, docker_cleanup.as_ref()).await;
        }
        SupervisorEvent::ReaderFailed(failure) => {
            termination = failure;
            cleanup =
                cleanup_execution_child(&mut child, process_id, docker_cleanup.as_ref()).await;
        }
    }

    let (collector_wait, collector_wait_failure) =
        if matches!(termination, ExecutionTerminationCause::Exited) {
            match deadline {
                Some(deadline) => {
                    let remaining = deadline.saturating_duration_since(TokioInstant::now());
                    (
                        Some(remaining.min(OUTPUT_DRAIN_GRACE)),
                        if remaining <= OUTPUT_DRAIN_GRACE {
                            ExecutionTerminationCause::TimedOut
                        } else {
                            output_reader_drain_failure()
                        },
                    )
                }
                None => (Some(OUTPUT_DRAIN_GRACE), output_reader_drain_failure()),
            }
        } else {
            (Some(OUTPUT_DRAIN_GRACE), termination.clone())
        };
    let cleanup_performed_before_join = !matches!(termination, ExecutionTerminationCause::Exited);
    let collections = join_pipe_tasks(
        &mut stdout_task,
        &mut stderr_task,
        collector_wait,
        collector_wait_failure,
    )
    .await;
    if let Err(failure) = collections {
        if matches!(termination, ExecutionTerminationCause::Exited) {
            termination = failure;
            cleanup =
                cleanup_execution_child(&mut child, process_id, docker_cleanup.as_ref()).await;
            if join_pipe_tasks(
                &mut stdout_task,
                &mut stderr_task,
                Some(OUTPUT_DRAIN_GRACE),
                output_reader_drain_failure(),
            )
            .await
            .is_err()
            {
                stdout_task.abort().await;
                stderr_task.abort().await;
            }
        } else if cleanup_performed_before_join {
            stdout_task.abort().await;
            stderr_task.abort().await;
        }
    }
    let (stdout, stderr) = (stdout_task.into_collection(), stderr_task.into_collection());
    if matches!(termination, ExecutionTerminationCause::Exited)
        && let Ok(alert) = alert_rx.try_recv()
    {
        termination = alert.termination();
        cleanup = cleanup_execution_child(&mut child, process_id, docker_cleanup.as_ref()).await;
    }
    if matches!(termination, ExecutionTerminationCause::Exited)
        && let Some(docker_cleanup) = docker_cleanup.as_ref()
    {
        cleanup = docker_cleanup
            .reconcile_after_cli_exit(&mut child, process_id, exit_code)
            .await;
    }
    let timed_out = matches!(termination, ExecutionTerminationCause::TimedOut);
    let resources = resource_receipt_for_request(request, timed_out, cleanup);
    let combined_total_bytes = stdout
        .evidence
        .total_bytes
        .saturating_add(stderr.evidence.total_bytes);

    Ok(ExecutionReceipt {
        backend,
        capabilities,
        network,
        resources,
        environment_policy: request.environment_policy,
        exit_code,
        stdout: stdout.bytes,
        stderr: stderr.bytes,
        output: ExecutionOutputReceipt {
            schema_version: EXECUTION_OUTPUT_RECEIPT_SCHEMA_VERSION,
            stdout: stdout.evidence,
            stderr: stderr.evidence,
            combined_total_bytes,
            combined_hard_limit_bytes: output_limits.hard_bytes_combined,
            termination,
        },
        timed_out,
    })
}

enum SupervisorEvent {
    Child(std::io::Result<std::process::ExitStatus>),
    Alert(OutputAlert),
    Timeout,
    Cancelled,
    ReaderFailed(ExecutionTerminationCause),
}

enum SupervisorWake {
    Child(std::io::Result<std::process::ExitStatus>),
    Alert(OutputAlert),
    Timeout,
    Cancelled,
    StdoutTask(std::result::Result<CollectedPipe, tokio::task::JoinError>),
    StderrTask(std::result::Result<CollectedPipe, tokio::task::JoinError>),
}

async fn wait_for_child_or_output_alert(
    child: &mut Child,
    alert_rx: &mut mpsc::Receiver<OutputAlert>,
    deadline: Option<TokioInstant>,
    stdout_task: &mut PipeTaskState,
    stderr_task: &mut PipeTaskState,
    cancellation: Option<&sigil_kernel::RunCancellationHandle>,
) -> SupervisorEvent {
    loop {
        let wake = match deadline {
            Some(deadline) => tokio::select! {
                biased;
                Some(alert) = alert_rx.recv() => SupervisorWake::Alert(alert),
                result = stdout_task.wait_for_completion(), if stdout_task.is_pending() => {
                    SupervisorWake::StdoutTask(result)
                }
                result = stderr_task.wait_for_completion(), if stderr_task.is_pending() => {
                    SupervisorWake::StderrTask(result)
                }
                status = child.wait() => SupervisorWake::Child(status),
                _ = wait_for_execution_cancellation(cancellation) => SupervisorWake::Cancelled,
                () = tokio::time::sleep_until(deadline) => SupervisorWake::Timeout,
            },
            None => tokio::select! {
                biased;
                Some(alert) = alert_rx.recv() => SupervisorWake::Alert(alert),
                result = stdout_task.wait_for_completion(), if stdout_task.is_pending() => {
                    SupervisorWake::StdoutTask(result)
                }
                result = stderr_task.wait_for_completion(), if stderr_task.is_pending() => {
                    SupervisorWake::StderrTask(result)
                }
                status = child.wait() => SupervisorWake::Child(status),
                _ = wait_for_execution_cancellation(cancellation) => SupervisorWake::Cancelled,
            },
        };
        match wake {
            SupervisorWake::Child(status) => return SupervisorEvent::Child(status),
            SupervisorWake::Alert(alert) => return SupervisorEvent::Alert(alert),
            SupervisorWake::Timeout => return SupervisorEvent::Timeout,
            SupervisorWake::Cancelled => return SupervisorEvent::Cancelled,
            SupervisorWake::StdoutTask(result) => {
                if let Some(failure) = stdout_task.record_completion(result) {
                    return SupervisorEvent::ReaderFailed(failure);
                }
            }
            SupervisorWake::StderrTask(result) => {
                if let Some(failure) = stderr_task.record_completion(result) {
                    return SupervisorEvent::ReaderFailed(failure);
                }
            }
        }
    }
}

async fn wait_for_execution_cancellation(
    cancellation: Option<&sigil_kernel::RunCancellationHandle>,
) {
    match cancellation {
        Some(cancellation) => cancellation.cancelled().await,
        None => std::future::pending::<()>().await,
    }
}

struct PipeTaskState {
    stream: sigil_kernel::ExecutionOutputStream,
    task: Option<JoinHandle<CollectedPipe>>,
    collection: Option<CollectedPipe>,
    failure: Option<ExecutionTerminationCause>,
    limits: OutputCollectionLimits,
    observed_total: Arc<AtomicU64>,
}

impl PipeTaskState {
    fn new(
        stream: sigil_kernel::ExecutionOutputStream,
        task: Option<JoinHandle<CollectedPipe>>,
        limits: OutputCollectionLimits,
        observed_total: Arc<AtomicU64>,
    ) -> Self {
        Self {
            stream,
            task,
            collection: None,
            failure: None,
            limits,
            observed_total,
        }
    }

    fn is_pending(&self) -> bool {
        self.task.is_some()
    }

    async fn wait_for_completion(
        &mut self,
    ) -> std::result::Result<CollectedPipe, tokio::task::JoinError> {
        match self.task.as_mut() {
            Some(task) => task.await,
            None => std::future::pending().await,
        }
    }

    fn record_completion(
        &mut self,
        result: std::result::Result<CollectedPipe, tokio::task::JoinError>,
    ) -> Option<ExecutionTerminationCause> {
        self.task = None;
        match result {
            Ok(collection) => {
                self.collection = Some(collection);
                None
            }
            Err(error) => {
                let failure = ExecutionTerminationCause::ReaderFailed {
                    stream: self.stream,
                    reason: format!("output reader task failed: {error}"),
                };
                self.failure = Some(failure.clone());
                Some(failure)
            }
        }
    }

    async fn join(&mut self) -> std::result::Result<(), ExecutionTerminationCause> {
        if self.collection.is_some() {
            return Ok(());
        }
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        let Some(task) = self.task.as_mut() else {
            self.collection = Some(empty_collection(
                self.limits,
                self.observed_total
                    .load(std::sync::atomic::Ordering::Relaxed),
            ));
            return Ok(());
        };
        let result = task.await;
        match self.record_completion(result) {
            Some(failure) => Err(failure),
            None => Ok(()),
        }
    }

    async fn abort(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
            if self.failure.is_none() {
                self.failure = Some(ExecutionTerminationCause::ReaderFailed {
                    stream: self.stream,
                    reason: "output reader did not stop after process-tree cleanup".to_owned(),
                });
            }
        }
    }

    fn into_collection(self) -> CollectedPipe {
        self.collection.unwrap_or_else(|| {
            empty_collection(
                self.limits,
                self.observed_total
                    .load(std::sync::atomic::Ordering::Relaxed),
            )
        })
    }
}

async fn join_pipe_tasks(
    stdout_task: &mut PipeTaskState,
    stderr_task: &mut PipeTaskState,
    max_wait: Option<Duration>,
    timeout_termination: ExecutionTerminationCause,
) -> std::result::Result<(), ExecutionTerminationCause> {
    let wait = async {
        let (stdout, stderr) = tokio::join!(stdout_task.join(), stderr_task.join());
        (stdout, stderr)
    };
    let (stdout, stderr) = match max_wait {
        Some(max_wait) => match tokio::time::timeout(max_wait, wait).await {
            Ok(result) => result,
            Err(_) => return Err(timeout_termination),
        },
        None => wait.await,
    };
    match (stdout, stderr) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(termination), _) => Err(termination),
        (_, Err(termination)) => Err(termination),
    }
}

fn empty_collection(limits: OutputCollectionLimits, observed_total_bytes: u64) -> CollectedPipe {
    CollectedPipe {
        bytes: Vec::new(),
        evidence: sigil_kernel::ExecutionStreamCapture {
            total_bytes: observed_total_bytes,
            omitted_bytes: observed_total_bytes,
            retained_limit_bytes: limits.retained_bytes_per_stream,
            hard_limit_bytes: limits.hard_bytes_per_stream,
            truncated: observed_total_bytes > 0,
            ..sigil_kernel::ExecutionStreamCapture::default()
        },
    }
}

fn output_reader_drain_failure() -> ExecutionTerminationCause {
    ExecutionTerminationCause::ReaderFailed {
        stream: sigil_kernel::ExecutionOutputStream::Combined,
        reason: "output readers did not close within the bounded drain grace".to_owned(),
    }
}

pub(crate) fn resource_receipt_for_request(
    request: &ExecutionRequest,
    timed_out: bool,
    cleanup: ExecutionCleanupReceipt,
) -> ExecutionResourceReceipt {
    let mut applied_limits = Vec::new();
    if let Some(timeout_ms) = request.timeout_millis() {
        applied_limits.push(ExecutionResourceLimitReceipt::new(
            ExecutionResourceLimitKind::WallClockTimeout,
            format!("{timeout_ms}ms"),
        ));
    }
    let mut unsupported_limits = Vec::new();
    if let Some(cpu_time_ms) = request.cpu_time_ms {
        unsupported_limits.push(ExecutionResourceLimitReceipt::new(
            ExecutionResourceLimitKind::CpuTime,
            format!("{cpu_time_ms}ms"),
        ));
    }
    if let Some(memory_limit_bytes) = request.memory_limit_bytes {
        unsupported_limits.push(ExecutionResourceLimitReceipt::new(
            ExecutionResourceLimitKind::Memory,
            format!("{memory_limit_bytes} bytes"),
        ));
    }
    if let Some(process_count_limit) = request.process_count_limit {
        unsupported_limits.push(ExecutionResourceLimitReceipt::new(
            ExecutionResourceLimitKind::ProcessCount,
            format!("{process_count_limit} processes"),
        ));
    }
    ExecutionResourceReceipt {
        applied_limits,
        unsupported_limits,
        timeout_source: if timed_out {
            ExecutionTimeoutSource::WallClock
        } else {
            ExecutionTimeoutSource::None
        },
        cleanup,
    }
}

async fn cleanup_execution_child(
    child: &mut tokio::process::Child,
    process_id: Option<u32>,
    docker_cleanup: Option<&docker::DockerContainerCleanup>,
) -> ExecutionCleanupReceipt {
    if let Some(docker_cleanup) = docker_cleanup {
        docker_cleanup.cleanup(child, process_id).await
    } else {
        cleanup_timed_out_child(child, process_id).await
    }
}

pub(crate) async fn cleanup_timed_out_child(
    child: &mut tokio::process::Child,
    process_id: Option<u32>,
) -> ExecutionCleanupReceipt {
    #[cfg(unix)]
    {
        return cleanup_unix_process_group(child, process_id).await;
    }

    #[cfg(windows)]
    {
        return cleanup_windows_process_tree(child, process_id).await;
    }

    #[cfg(not(any(unix, windows)))]
    cleanup_unsupported_process_tree(child, process_id).await
}

#[cfg(unix)]
async fn cleanup_unix_process_group(
    child: &mut tokio::process::Child,
    process_id: Option<u32>,
) -> ExecutionCleanupReceipt {
    let Some(process_id) = process_id else {
        let direct_kill = child.start_kill();
        let direct_wait = tokio::time::timeout(PROCESS_KILL_VERIFY_GRACE, child.wait()).await;
        return ExecutionCleanupReceipt::failed(format!(
            "process id unavailable; process-group cleanup could not be proven: direct_kill={direct_kill:?}, direct_wait={direct_wait:?}"
        ));
    };

    let direct_already_reaped = match child.try_wait() {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(error) => {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(PROCESS_KILL_VERIFY_GRACE, child.wait()).await;
            return ExecutionCleanupReceipt::failed(format!(
                "failed to inspect child before process-group cleanup: {error}; direct-child fallback attempted"
            ));
        }
    };
    let term_error = if direct_already_reaped {
        None
    } else {
        send_signal_to_process_group(process_id, "TERM")
            .await
            .err()
            .map(|error| error.to_string())
    };
    let direct_reaped = if direct_already_reaped {
        true
    } else {
        match tokio::time::timeout(PROCESS_TERM_GRACE, child.wait()).await {
            Ok(Ok(_)) => true,
            Ok(Err(error)) => {
                let direct_kill = child.start_kill();
                let direct_wait =
                    tokio::time::timeout(PROCESS_KILL_VERIFY_GRACE, child.wait()).await;
                return ExecutionCleanupReceipt::failed(format!(
                    "process-group cleanup wait failed: {error}; direct_kill={direct_kill:?}, direct_wait={direct_wait:?}"
                ));
            }
            Err(_) => false,
        }
    };

    let group_alive = match process_group_is_alive(process_id).await {
        Ok(alive) => alive,
        Err(error) => {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(PROCESS_KILL_VERIFY_GRACE, child.wait()).await;
            return ExecutionCleanupReceipt::failed(format!(
                "failed to inspect process group {process_id}: {error}; direct-child fallback attempted"
            ));
        }
    };
    if group_alive && let Err(error) = send_signal_to_process_group(process_id, "KILL").await {
        match process_group_is_alive(process_id).await {
            Ok(false) => {}
            Ok(true) | Err(_) => {
                let direct_kill = child.start_kill();
                let direct_wait =
                    tokio::time::timeout(PROCESS_KILL_VERIFY_GRACE, child.wait()).await;
                return ExecutionCleanupReceipt::failed(format!(
                    "failed to send SIGKILL to process group {process_id}: {error}; direct_kill={direct_kill:?}, direct_wait={direct_wait:?}"
                ));
            }
        }
    }
    if !direct_reaped {
        match tokio::time::timeout(PROCESS_KILL_VERIFY_GRACE, child.wait()).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                return ExecutionCleanupReceipt::failed(format!(
                    "process group {process_id} was killed but direct child reap failed: {error}"
                ));
            }
            Err(_) => {
                let _ = child.start_kill();
                return ExecutionCleanupReceipt::failed(format!(
                    "process group {process_id} was killed but direct child reap timed out"
                ));
            }
        }
    }

    let verify_deadline = TokioInstant::now() + PROCESS_KILL_VERIFY_GRACE;
    while TokioInstant::now() < verify_deadline {
        match process_group_is_alive(process_id).await {
            Ok(false) => {
                let term_detail = term_error
                    .map(|error| format!("; SIGTERM failed before forced cleanup: {error}"))
                    .unwrap_or_default();
                return ExecutionCleanupReceipt::completed(format!(
                    "process group {process_id} terminated and direct child reaped{term_detail}"
                ));
            }
            Ok(true) => tokio::time::sleep(Duration::from_millis(10)).await,
            Err(error) => {
                return ExecutionCleanupReceipt::failed(format!(
                    "failed to verify process group {process_id} cleanup: {error}"
                ));
            }
        }
    }
    ExecutionCleanupReceipt::failed(format!(
        "process group {process_id} still exists after SIGKILL and direct child reap"
    ))
}

#[cfg(windows)]
async fn cleanup_windows_process_tree(
    child: &mut tokio::process::Child,
    process_id: Option<u32>,
) -> ExecutionCleanupReceipt {
    let Some(process_id) = process_id else {
        let direct_kill = child.start_kill();
        let direct_wait = tokio::time::timeout(PROCESS_KILL_VERIFY_GRACE, child.wait()).await;
        return ExecutionCleanupReceipt::unsupported(format!(
            "process id unavailable; Windows process-tree cleanup could not be requested: direct_kill={direct_kill:?}, direct_wait={direct_wait:?}"
        ));
    };
    let mut taskkill_command = Command::new("taskkill");
    taskkill_command.args(["/PID", &process_id.to_string(), "/T", "/F"]);
    let taskkill = run_cleanup_command(
        taskkill_command,
        format!("taskkill process tree {process_id}"),
    )
    .await;
    if !matches!(&taskkill, Ok(status) if status.success()) {
        let _ = child.start_kill();
    }
    let wait = tokio::time::timeout(PROCESS_KILL_VERIFY_GRACE, child.wait()).await;
    match (&taskkill, &wait) {
        (Ok(status), Ok(Ok(_))) if status.success() => ExecutionCleanupReceipt::completed(format!(
            "taskkill terminated process tree {process_id} and direct child was reaped"
        )),
        _ => ExecutionCleanupReceipt::failed(format!(
            "Windows process-tree cleanup could not be proven: taskkill={taskkill:?}, wait={wait:?}"
        )),
    }
}

#[cfg(not(any(unix, windows)))]
async fn cleanup_unsupported_process_tree(
    child: &mut tokio::process::Child,
    process_id: Option<u32>,
) -> ExecutionCleanupReceipt {
    let kill = child.start_kill();
    let wait = tokio::time::timeout(PROCESS_KILL_VERIFY_GRACE, child.wait()).await;
    ExecutionCleanupReceipt::unsupported(format!(
        "platform has no process-tree cleanup implementation; direct child cleanup only: pid={process_id:?}, kill={kill:?}, wait={wait:?}"
    ))
}

async fn run_cleanup_command(
    mut command: Command,
    description: impl Into<String>,
) -> Result<std::process::ExitStatus> {
    let description = description.into();
    command
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match tokio::time::timeout(PROCESS_CLEANUP_COMMAND_TIMEOUT, command.status()).await {
        Ok(status) => status.with_context(|| format!("failed to invoke {description}")),
        Err(_) => bail!(
            "{description} exceeded the {:?} cleanup-command deadline",
            PROCESS_CLEANUP_COMMAND_TIMEOUT
        ),
    }
}

pub(crate) fn configure_execution_process_group(command: &mut Command) {
    #[cfg(unix)]
    command.process_group(0);
}
