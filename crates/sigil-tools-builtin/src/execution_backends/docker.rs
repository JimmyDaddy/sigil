use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use sigil_kernel::{
    ExecutionBackend, ExecutionBackendCapabilities, ExecutionBackendKind, ExecutionFuture,
    ExecutionNetworkReceipt, ExecutionReceipt, ExecutionRequest,
};
use tokio::process::Command;

use super::{
    command_output_to_receipt, command_output_with_timeout, configure_command_environment,
};

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
        Box::pin(async move { docker_execute(docker, image, network_allowed, request).await })
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
) -> Result<ExecutionReceipt> {
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
    command_output_to_receipt(
        ExecutionBackendKind::Docker,
        DockerExecutionBackend::new(docker, String::new(), network_allowed).capabilities(),
        network,
        command,
        &request,
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
    let output = Command::new(program).args(args).output().await?;
    if !output.status.success() {
        bail!(
            "{program} {} failed with exit {:?}",
            args.join(" "),
            output.status.code()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
