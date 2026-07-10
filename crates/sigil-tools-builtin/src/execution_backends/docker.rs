use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use sigil_kernel::{
    ExecutionBackend, ExecutionBackendCapabilities, ExecutionBackendKind, ExecutionCleanupReceipt,
    ExecutionCleanupStatus, ExecutionFuture, ExecutionNetworkReceipt, ExecutionReceipt,
    ExecutionRequest, ExecutionTerminationCause, ProcessEnvironmentPolicy,
};
use tempfile::TempDir;
use tokio::{io::AsyncReadExt, process::Command, time::Instant as TokioInstant};

use super::{
    bounded_short_command_output, bounded_short_path_command_output_with_environment,
    command_output_to_receipt_with_docker_cleanup, command_output_with_timeout,
    configure_command_environment,
};

const DOCKER_CONTAINER_ID_BYTES: usize = 64;
const DOCKER_CONTAINER_ID_READ_BYTES: usize = 128;
const DOCKER_CID_WAIT_GRACE: Duration = Duration::from_millis(500);
const DOCKER_CONTROL_TIMEOUT: Duration = Duration::from_secs(2);

pub(super) struct DockerContainerCleanup {
    docker: PathBuf,
    cid_file: PathBuf,
    environment_policy: ProcessEnvironmentPolicy,
    environment: BTreeMap<String, String>,
    _cid_directory: TempDir,
}

impl DockerContainerCleanup {
    fn new(docker: PathBuf, request: &ExecutionRequest) -> Result<Self> {
        let cid_directory = tempfile::Builder::new()
            .prefix("sigil-docker-cid-")
            .tempdir()
            .context("failed to create private Docker container identity directory")?;
        let cid_file = cid_directory.path().join("container.cid");
        Ok(Self {
            docker,
            cid_file,
            environment_policy: request.environment_policy,
            environment: request.env.clone(),
            _cid_directory: cid_directory,
        })
    }

    fn cid_file(&self) -> &Path {
        &self.cid_file
    }

    pub(super) async fn cleanup(
        &self,
        child: &mut tokio::process::Child,
        process_id: Option<u32>,
    ) -> ExecutionCleanupReceipt {
        let initial_id = self.wait_for_container_id(DOCKER_CID_WAIT_GRACE).await;
        let mut container_id = initial_id.as_ref().ok().and_then(Clone::clone);
        let mut docker_diagnostics = Vec::new();
        if let Some(container_id) = container_id.as_deref() {
            docker_diagnostics.push(self.force_remove(container_id).await);
        } else if let Err(error) = &initial_id {
            docker_diagnostics.push(format!("initial container identity read failed: {error}"));
        }

        // The daemon-owned container is outside the docker CLI process group. Host cleanup must
        // still run even when cidfile parsing or Docker control commands fail.
        let host_cleanup = super::cleanup_timed_out_child(child, process_id).await;

        if container_id.is_none() {
            match self.wait_for_container_id(DOCKER_CID_WAIT_GRACE).await {
                Ok(id) => container_id = id,
                Err(error) => {
                    docker_diagnostics
                        .push(format!("post-reap container identity read failed: {error}"));
                }
            }
        }
        if let Some(container_id) = container_id.as_deref() {
            docker_diagnostics.push(self.force_remove(container_id).await);
        }

        let container_stopped = match container_id.as_deref() {
            Some(container_id) => match self.container_is_running(container_id).await {
                Ok(false) => true,
                Ok(true) => {
                    docker_diagnostics.push(
                        "bounded Docker query still reports the container as running".to_owned(),
                    );
                    false
                }
                Err(error) => {
                    docker_diagnostics
                        .push(format!("failed to verify Docker container state: {error}"));
                    false
                }
            },
            None => {
                docker_diagnostics.push(
                    "Docker container identity was unavailable; daemon cleanup cannot be proven"
                        .to_owned(),
                );
                false
            }
        };
        self.finish_cleanup(host_cleanup, container_stopped, docker_diagnostics)
    }

    pub(super) async fn reconcile_after_cli_exit(
        &self,
        child: &mut tokio::process::Child,
        process_id: Option<u32>,
        exit_code: Option<i32>,
    ) -> ExecutionCleanupReceipt {
        let container_id = match self.wait_for_container_id(DOCKER_CID_WAIT_GRACE).await {
            Ok(Some(container_id)) => container_id,
            Ok(None) => {
                let host_cleanup = super::cleanup_timed_out_child(child, process_id).await;
                return ExecutionCleanupReceipt::failed(format!(
                    "Docker CLI exited with {exit_code:?} without writing a container identity; a daemon create/cidfile race cannot be excluded; host={host_cleanup:?}"
                ));
            }
            Err(error) => {
                let host_cleanup = super::cleanup_timed_out_child(child, process_id).await;
                return ExecutionCleanupReceipt::failed(format!(
                    "Docker container identity was invalid after CLI exit: {error}; host={host_cleanup:?}"
                ));
            }
        };

        match self.container_is_running(&container_id).await {
            Ok(false) => ExecutionCleanupReceipt {
                status: ExecutionCleanupStatus::NotNeeded,
                reason: Some(
                    "bounded Docker query confirmed the execution container is no longer running"
                        .to_owned(),
                ),
            },
            Ok(true) => {
                let mut docker_diagnostics = vec![
                    "Docker CLI exited while its daemon container was still running".to_owned(),
                    self.force_remove(&container_id).await,
                ];
                let host_cleanup = super::cleanup_timed_out_child(child, process_id).await;
                docker_diagnostics.push(self.force_remove(&container_id).await);
                let container_stopped = match self.container_is_running(&container_id).await {
                    Ok(false) => true,
                    Ok(true) => {
                        docker_diagnostics.push(
                            "bounded Docker query still reports the container as running"
                                .to_owned(),
                        );
                        false
                    }
                    Err(error) => {
                        docker_diagnostics
                            .push(format!("failed to verify Docker container state: {error}"));
                        false
                    }
                };
                self.finish_cleanup(host_cleanup, container_stopped, docker_diagnostics)
            }
            Err(error) => {
                let host_cleanup = super::cleanup_timed_out_child(child, process_id).await;
                ExecutionCleanupReceipt::failed(format!(
                    "failed to reconcile Docker container state after CLI exit: {error}; host={host_cleanup:?}"
                ))
            }
        }
    }

    fn finish_cleanup(
        &self,
        host_cleanup: ExecutionCleanupReceipt,
        container_stopped: bool,
        docker_diagnostics: Vec<String>,
    ) -> ExecutionCleanupReceipt {
        let host_completed = host_cleanup.status == ExecutionCleanupStatus::Completed;
        let host_status = host_cleanup.status;
        let host_reason = host_cleanup
            .reason
            .unwrap_or_else(|| format!("host cleanup status={host_status:?}"));
        let docker_reason = docker_diagnostics.join("; ");
        if host_completed && container_stopped {
            ExecutionCleanupReceipt::completed(format!(
                "Docker container is no longer running and docker CLI cleanup completed: host={host_reason}; docker={docker_reason}"
            ))
        } else {
            ExecutionCleanupReceipt::failed(format!(
                "Docker execution cleanup was not fully proven: host_status={host_status:?}, host={host_reason}; docker={docker_reason}"
            ))
        }
    }

    async fn wait_for_container_id(&self, max_wait: Duration) -> Result<Option<String>> {
        let deadline = TokioInstant::now()
            .checked_add(max_wait)
            .unwrap_or_else(TokioInstant::now);
        let mut last_error = None;
        loop {
            match self.read_container_id().await {
                Ok(Some(container_id)) => return Ok(Some(container_id)),
                Ok(None) => {}
                Err(error) => last_error = Some(error),
            }
            if TokioInstant::now() >= deadline {
                return match last_error {
                    Some(error) => Err(error),
                    None => Ok(None),
                };
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn read_container_id(&self) -> Result<Option<String>> {
        let mut file = match tokio::fs::File::open(&self.cid_file).await {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("failed to open Docker cidfile"),
        };
        let mut bytes = [0_u8; DOCKER_CONTAINER_ID_READ_BYTES + 1];
        let mut read = 0;
        while read < bytes.len() {
            let chunk = file
                .read(&mut bytes[read..])
                .await
                .context("failed to read Docker cidfile")?;
            if chunk == 0 {
                break;
            }
            read += chunk;
        }
        if read > DOCKER_CONTAINER_ID_READ_BYTES {
            bail!("Docker cidfile exceeds the bounded identity size");
        }
        let container_id = std::str::from_utf8(&bytes[..read])
            .context("Docker cidfile is not valid UTF-8")?
            .trim();
        if container_id.is_empty() {
            return Ok(None);
        }
        if container_id.len() != DOCKER_CONTAINER_ID_BYTES
            || !container_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            bail!("Docker cidfile does not contain one full container id");
        }
        Ok(Some(container_id.to_ascii_lowercase()))
    }

    async fn force_remove(&self, container_id: &str) -> String {
        let mut command = Command::new(&self.docker);
        command.args(["container", "rm", "--force", container_id]);
        self.configure_control_command(&mut command);
        match super::run_cleanup_command(command, "force-remove Docker execution container").await {
            Ok(status) if status.success() => "docker rm --force succeeded".to_owned(),
            Ok(status) => format!("docker rm --force exited with {status}"),
            Err(error) => format!("docker rm --force failed: {error}"),
        }
    }

    async fn container_is_running(&self, container_id: &str) -> Result<bool> {
        let filter = format!("id={container_id}");
        let args = [
            OsString::from("container"),
            OsString::from("ls"),
            OsString::from("--quiet"),
            OsString::from("--no-trunc"),
            OsString::from("--filter"),
            OsString::from(filter),
        ];
        let receipt = Box::pin(bounded_short_path_command_output_with_environment(
            &self.docker,
            &args,
            DOCKER_CONTROL_TIMEOUT,
            self.environment_policy,
            &self.environment,
        ))
        .await
        .context("failed to query running Docker containers")?;
        let output = receipt.effective_output();
        if !matches!(output.termination, ExecutionTerminationCause::Exited) {
            bail!(
                "Docker container query ended with {}",
                output.termination.as_str()
            );
        }
        if receipt.exit_code != Some(0) {
            bail!("Docker container query exited with {:?}", receipt.exit_code);
        }
        if output.stdout.truncated || output.stderr.truncated {
            bail!("Docker container query output was truncated");
        }
        let stdout = std::str::from_utf8(&receipt.stdout)
            .context("Docker container query output is not valid UTF-8")?;
        let mut ids = stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty());
        match (ids.next(), ids.next()) {
            (None, None) => Ok(false),
            (Some(id), None) if id.eq_ignore_ascii_case(container_id) => Ok(true),
            _ => bail!("Docker container query returned an unexpected identity set"),
        }
    }

    fn configure_control_command(&self, command: &mut Command) {
        if self.environment_policy.clears_parent() {
            command.env_clear();
        }
        command.envs(&self.environment);
    }
}

#[derive(Debug, Clone)]
pub struct DockerExecutionBackend {
    docker: PathBuf,
    image: String,
    network_allowed: bool,
}

impl DockerExecutionBackend {
    #[must_use]
    pub fn new(docker: PathBuf, image: String, network_allowed: bool) -> Self {
        Self {
            docker,
            image,
            network_allowed,
        }
    }

    #[must_use]
    pub fn is_available(&self) -> bool {
        self.docker.is_file()
    }

    #[must_use]
    pub fn image(&self) -> &str {
        &self.image
    }
}

impl ExecutionBackend for DockerExecutionBackend {
    fn kind(&self) -> ExecutionBackendKind {
        ExecutionBackendKind::Docker
    }

    fn capabilities(&self) -> ExecutionBackendCapabilities {
        ExecutionBackendCapabilities {
            filesystem_isolation: true,
            network_isolation: true,
            process_isolation: true,
            resource_limits: false,
            persistent_pty: false,
            workspace_snapshot: false,
        }
    }

    fn planned_network_receipt(&self) -> ExecutionNetworkReceipt {
        if self.network_allowed {
            ExecutionNetworkReceipt::allowed("profile allows network access")
        } else {
            ExecutionNetworkReceipt::denied("docker launch plan uses --network none")
        }
    }

    fn execute(&self, request: ExecutionRequest) -> ExecutionFuture<'_> {
        let docker = self.docker.clone();
        let image = self.image.clone();
        let network_allowed = self.network_allowed;
        Box::pin(async move { docker_execute(docker, image, network_allowed, request, None).await })
    }

    fn execute_with_cancellation(
        &self,
        request: ExecutionRequest,
        cancellation: Option<sigil_kernel::RunCancellationHandle>,
    ) -> ExecutionFuture<'_> {
        let docker = self.docker.clone();
        let image = self.image.clone();
        let network_allowed = self.network_allowed;
        Box::pin(async move {
            docker_execute(docker, image, network_allowed, request, cancellation).await
        })
    }
}

pub(crate) fn ensure_docker_available(backend: &DockerExecutionBackend) -> Result<()> {
    if !backend.is_available() {
        bail!(
            "docker execution backend requires docker executable at {}",
            backend.docker.display()
        );
    }
    docker_check(
        &backend.docker,
        &["version", "--format", "{{.Server.Version}}"],
        "docker daemon is unavailable",
    )?;
    docker_check(
        &backend.docker,
        &["image", "inspect", backend.image()],
        &format!(
            "docker execution backend requires configured image {}",
            backend.image()
        ),
    )?;
    Ok(())
}

pub(crate) fn docker_check(docker: &Path, args: &[&str], failure_context: &str) -> Result<()> {
    let output = command_output_with_timeout(docker, args, Duration::from_secs(3))
        .with_context(|| format!("failed to run docker availability check: {failure_context}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if !stderr.trim().is_empty() {
            stderr.trim()
        } else {
            stdout.trim()
        };
        bail!("{failure_context}: {detail}");
    }
    Ok(())
}

pub(crate) async fn docker_execute(
    docker: PathBuf,
    image: String,
    network_allowed: bool,
    request: ExecutionRequest,
    cancellation: Option<sigil_kernel::RunCancellationHandle>,
) -> Result<ExecutionReceipt> {
    let docker_cleanup = DockerContainerCleanup::new(docker.clone(), &request)?;
    let canonical_cwd = fs::canonicalize(&request.cwd)
        .with_context(|| format!("failed to canonicalize cwd {}", request.cwd.display()))?;
    let mount = format!(
        "type=bind,src={},dst={}",
        canonical_cwd.display(),
        canonical_cwd.display()
    );
    let mut command = Command::new(&docker);
    command
        .arg("run")
        .arg("--rm")
        .arg("--cidfile")
        .arg(docker_cleanup.cid_file())
        .arg("--workdir")
        .arg(&canonical_cwd)
        .arg("--mount")
        .arg(mount);
    if !network_allowed {
        command.arg("--network").arg("none");
    }
    if let Some(user) = current_user_group_flag().await? {
        command.arg("--user").arg(user);
    }
    for (key, value) in &request.env {
        command.arg("--env").arg(format!("{key}={value}"));
    }
    command
        .arg(image)
        .arg(&request.program)
        .args(&request.args)
        .kill_on_drop(true);
    configure_command_environment(&mut command, &request);

    let network = if network_allowed {
        ExecutionNetworkReceipt::allowed("profile allows network access")
    } else {
        ExecutionNetworkReceipt::denied("docker run uses --network none")
    };
    command_output_to_receipt_with_docker_cleanup(
        ExecutionBackendKind::Docker,
        DockerExecutionBackend::new(docker, String::new(), network_allowed).capabilities(),
        network,
        command,
        &request,
        docker_cleanup,
        cancellation,
    )
    .await
}

pub(crate) async fn current_user_group_flag() -> Result<Option<String>> {
    if !cfg!(unix) {
        return Ok(None);
    }
    let uid = short_command_output("id", &["-u"]).await?;
    let gid = short_command_output("id", &["-g"]).await?;
    Ok(Some(format!("{uid}:{gid}")))
}

pub(crate) async fn short_command_output(program: &str, args: &[&str]) -> Result<String> {
    let receipt = bounded_short_command_output(program, args, Duration::from_secs(3)).await?;
    let output = receipt.effective_output();
    if !matches!(
        output.termination,
        sigil_kernel::ExecutionTerminationCause::Exited
    ) {
        bail!(
            "{program} {} failed during bounded output collection: {}",
            args.join(" "),
            output.termination.as_str()
        );
    }
    if receipt.exit_code != Some(0) {
        bail!(
            "{program} {} failed with exit {:?}",
            args.join(" "),
            receipt.exit_code
        );
    }
    Ok(String::from_utf8_lossy(&receipt.stdout).trim().to_owned())
}
