use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command as StdCommand, Stdio},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use sigil_kernel::{
    ExecutionBackend, ExecutionBackendCapabilities, ExecutionBackendKind,
    ExecutionBackendSelectionDiagnostic, ExecutionCleanupReceipt, ExecutionConfig,
    ExecutionNetworkReceipt, ExecutionReceipt, ExecutionRequest, ExecutionResourceLimitKind,
    ExecutionResourceLimitReceipt, ExecutionResourceReceipt, ExecutionSandboxFallback,
    ExecutionSandboxProfile, ExecutionTimeoutSource,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};

mod bubblewrap;
mod docker;
mod local;
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

#[cfg(test)]
pub(crate) use docker::current_user_group_flag;

/// Command plan for a long-lived stdio process that needs the same sandbox policy as built-in tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LongLivedStdioProcessPlan {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub backend: ExecutionBackendKind,
    pub backend_capabilities: ExecutionBackendCapabilities,
    pub sandbox_profile: ExecutionSandboxProfile,
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
    env: &BTreeMap<String, String>,
) -> Result<LongLivedStdioProcessPlan> {
    if program.trim().is_empty() {
        bail!("long-lived stdio process command must not be empty");
    }
    let canonical_cwd = fs::canonicalize(cwd)
        .with_context(|| format!("failed to canonicalize cwd {}", cwd.display()))?;
    match config.backend() {
        ExecutionBackendKind::Local => {
            if config.requires_sandbox() {
                bail!(
                    "MCP stdio sandbox unavailable: local execution backend cannot enforce local stdio sandbox"
                );
            }
            Ok(LongLivedStdioProcessPlan {
                program: PathBuf::from(program),
                args: args.iter().map(OsString::from).collect(),
                cwd: canonical_cwd,
                env: env.clone(),
                backend: ExecutionBackendKind::Local,
                backend_capabilities: ExecutionBackendCapabilities::default(),
                sandbox_profile: ExecutionSandboxProfile::Unconfined,
                sandboxed: false,
            })
        }
        ExecutionBackendKind::MacosSeatbelt => {
            let backend = MacosSeatbeltExecutionBackend::default()
                .with_network_allowed(config.profile_spec().network_allowed);
            let capabilities = backend.capabilities();
            validate_long_lived_stdio_capabilities(config, capabilities)?;
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
                env: env.clone(),
                backend: ExecutionBackendKind::MacosSeatbelt,
                backend_capabilities: capabilities,
                sandbox_profile: config.profile(),
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
            validate_long_lived_stdio_capabilities(config, capabilities)?;
            if let Err(error) = ensure_linux_bubblewrap_available(&backend) {
                bail!("MCP stdio sandbox unavailable: {error}");
            }
            let request = ExecutionRequest {
                program: program.to_owned(),
                args: args.to_vec(),
                cwd: canonical_cwd.clone(),
                env: env.clone(),
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
                env: env.clone(),
                backend: ExecutionBackendKind::LinuxBubblewrap,
                backend_capabilities: capabilities,
                sandbox_profile: config.profile(),
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
) -> Result<()> {
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

pub(crate) fn command_output_with_timeout(
    program: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<std::process::Output> {
    let mut child = StdCommand::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let started = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            return child.wait_with_output().map_err(Into::into);
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let output = child.wait_with_output()?;
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "{} {} timed out after {}ms{}",
                program.display(),
                args.join(" "),
                timeout.as_millis(),
                if stderr.trim().is_empty() {
                    String::new()
                } else {
                    format!(": {}", stderr.trim())
                }
            );
        }
        thread::sleep(Duration::from_millis(20));
    }
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

pub(crate) async fn command_output_to_receipt(
    backend: ExecutionBackendKind,
    capabilities: ExecutionBackendCapabilities,
    network: ExecutionNetworkReceipt,
    mut command: Command,
    request: &ExecutionRequest,
) -> Result<ExecutionReceipt> {
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    configure_execution_process_group(&mut command);

    let mut child = command.spawn()?;
    let process_id = child.id();
    let stdout_task = child
        .stdout
        .take()
        .map(|stdout| tokio::spawn(read_pipe_to_end(stdout)));
    let stderr_task = child
        .stderr
        .take()
        .map(|stderr| tokio::spawn(read_pipe_to_end(stderr)));

    let (exit_code, timed_out, cleanup) = match request.timeout_duration() {
        Some(timeout) => match tokio::time::timeout(timeout, child.wait()).await {
            Ok(status) => (status?.code(), false, ExecutionCleanupReceipt::not_needed()),
            Err(_) => {
                let cleanup = cleanup_timed_out_child(&mut child, process_id).await;
                (None, true, cleanup)
            }
        },
        None => (
            child.wait().await?.code(),
            false,
            ExecutionCleanupReceipt::not_needed(),
        ),
    };
    let stdout = join_pipe_task(stdout_task).await?;
    let stderr = join_pipe_task(stderr_task).await?;
    let resources = resource_receipt_for_request(request, timed_out, cleanup);

    Ok(ExecutionReceipt {
        backend,
        capabilities,
        network,
        resources,
        exit_code,
        stdout,
        stderr,
        timed_out,
    })
}

pub(crate) async fn read_pipe_to_end<R>(mut pipe: R) -> Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut output = Vec::new();
    pipe.read_to_end(&mut output).await?;
    Ok(output)
}

pub(crate) async fn join_pipe_task(
    task: Option<tokio::task::JoinHandle<Result<Vec<u8>>>>,
) -> Result<Vec<u8>> {
    match task {
        Some(task) => task.await.context("execution output reader task failed")?,
        None => Ok(Vec::new()),
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

pub(crate) async fn cleanup_timed_out_child(
    child: &mut tokio::process::Child,
    process_id: Option<u32>,
) -> ExecutionCleanupReceipt {
    #[cfg(unix)]
    if let Some(process_id) = process_id
        && let Err(error) = send_signal_to_process_group(process_id, "TERM").await
    {
        let kill_result = force_kill_child(child, Some(process_id)).await;
        return ExecutionCleanupReceipt::failed(format!(
            "failed to send SIGTERM to process group {process_id}: {error}; {}",
            cleanup_result_reason(&kill_result)
        ));
    }

    match tokio::time::timeout(Duration::from_millis(500), child.wait()).await {
        Ok(Ok(_)) => ExecutionCleanupReceipt::completed("process exited after timeout cleanup"),
        Ok(Err(error)) => {
            ExecutionCleanupReceipt::failed(format!("process cleanup wait failed: {error}"))
        }
        Err(_) => force_kill_after_grace(child, process_id).await,
    }
}

pub(crate) async fn force_kill_after_grace(
    child: &mut tokio::process::Child,
    process_id: Option<u32>,
) -> ExecutionCleanupReceipt {
    #[cfg(unix)]
    if let Some(process_id) = process_id {
        match send_signal_to_process_group(process_id, "KILL").await {
            Ok(()) => match child.wait().await {
                Ok(_) => {
                    return ExecutionCleanupReceipt::completed(format!(
                        "sent SIGKILL to process group {process_id}"
                    ));
                }
                Err(error) => {
                    return ExecutionCleanupReceipt::failed(format!(
                        "sent SIGKILL to process group {process_id}; wait failed: {error}"
                    ));
                }
            },
            Err(error) => {
                let fallback = force_kill_child(child, Some(process_id)).await;
                return ExecutionCleanupReceipt::failed(format!(
                    "failed to send SIGKILL to process group {process_id}: {error}; {}",
                    cleanup_result_reason(&fallback)
                ));
            }
        }
    }
    force_kill_child(child, process_id).await
}

pub(crate) async fn force_kill_child(
    child: &mut tokio::process::Child,
    process_id: Option<u32>,
) -> ExecutionCleanupReceipt {
    match child.start_kill() {
        Ok(()) => match child.wait().await {
            Ok(_) => ExecutionCleanupReceipt::completed(format!(
                "killed child process {}",
                process_id.unwrap_or_default()
            )),
            Err(error) => {
                ExecutionCleanupReceipt::failed(format!("child process kill wait failed: {error}"))
            }
        },
        Err(error) => {
            ExecutionCleanupReceipt::failed(format!("failed to kill child process: {error}"))
        }
    }
}

pub(crate) fn cleanup_result_reason(receipt: &ExecutionCleanupReceipt) -> String {
    receipt
        .reason
        .clone()
        .unwrap_or_else(|| format!("fallback cleanup status {:?}", receipt.status))
}

#[cfg(unix)]
pub(crate) async fn send_signal_to_process_group(process_id: u32, signal: &str) -> Result<()> {
    let status = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(format!("-{process_id}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status()
        .await
        .with_context(|| format!("failed to invoke kill -{signal} -{process_id}"))?;
    if status.success() {
        Ok(())
    } else {
        bail!("kill -{signal} -{process_id} exited with {status}");
    }
}

pub(crate) fn configure_execution_process_group(command: &mut Command) {
    #[cfg(unix)]
    command.process_group(0);
}
