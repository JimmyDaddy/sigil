use std::{
    ffi::OsString,
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

use crate::constants::SIGIL_SCRATCH_DIR_ENV;

use super::{command_output_to_receipt, command_output_with_timeout};

#[derive(Debug, Clone)]
pub struct LinuxBubblewrapExecutionBackend {
    bwrap: PathBuf,
    network_allowed: bool,
}

impl Default for LinuxBubblewrapExecutionBackend {
    fn default() -> Self {
        Self {
            bwrap: PathBuf::from("bwrap"),
            network_allowed: false,
        }
    }
}

impl LinuxBubblewrapExecutionBackend {
    #[must_use]
    pub fn new(bwrap: PathBuf, network_allowed: bool) -> Self {
        Self {
            bwrap,
            network_allowed,
        }
    }

    #[must_use]
    pub fn bwrap_path(&self) -> &Path {
        &self.bwrap
    }

    #[must_use]
    pub fn is_available(&self) -> bool {
        cfg!(target_os = "linux") && self.bwrap.is_file()
    }
}

impl ExecutionBackend for LinuxBubblewrapExecutionBackend {
    fn kind(&self) -> ExecutionBackendKind {
        ExecutionBackendKind::LinuxBubblewrap
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

    fn execute(&self, request: ExecutionRequest) -> ExecutionFuture<'_> {
        let bwrap = self.bwrap.clone();
        let network_allowed = self.network_allowed;
        Box::pin(async move { linux_bubblewrap_execute(bwrap, network_allowed, request).await })
    }
}

pub(crate) fn ensure_linux_bubblewrap_available(
    backend: &LinuxBubblewrapExecutionBackend,
) -> Result<()> {
    if !cfg!(target_os = "linux") {
        bail!("linux_bubblewrap execution backend is only available on Linux");
    }
    if !backend.is_available() {
        bail!(
            "linux_bubblewrap execution backend requires {}",
            backend.bwrap_path().display()
        );
    }
    bubblewrap_check(
        backend.bwrap_path(),
        &["--version"],
        "bubblewrap executable is unavailable",
    )?;
    bubblewrap_check(
        backend.bwrap_path(),
        &[
            "--die-with-parent",
            "--unshare-pid",
            "--ro-bind",
            "/",
            "/",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "/bin/true",
        ],
        "bubblewrap namespace smoke test failed",
    )?;
    Ok(())
}

pub(crate) fn bubblewrap_check(bwrap: &Path, args: &[&str], failure_context: &str) -> Result<()> {
    let output =
        command_output_with_timeout(bwrap, args, Duration::from_secs(3)).with_context(|| {
            format!("failed to run bubblewrap availability check: {failure_context}")
        })?;
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

pub(crate) async fn linux_bubblewrap_execute(
    bwrap: PathBuf,
    network_allowed: bool,
    request: ExecutionRequest,
) -> Result<ExecutionReceipt> {
    if !cfg!(target_os = "linux") {
        bail!("linux_bubblewrap execution backend is only available on Linux");
    }
    if !bwrap.is_file() {
        bail!(
            "linux_bubblewrap execution backend requires {}",
            bwrap.display()
        );
    }

    let canonical_cwd = fs::canonicalize(&request.cwd)
        .with_context(|| format!("failed to canonicalize cwd {}", request.cwd.display()))?;
    let capabilities = LinuxBubblewrapExecutionBackend::default().capabilities();

    let mut command = Command::new(&bwrap);
    command
        .args(linux_bubblewrap_args(
            &canonical_cwd,
            &request,
            network_allowed,
        ))
        .arg(&request.program)
        .args(&request.args)
        .current_dir(&canonical_cwd)
        .envs(&request.env)
        .kill_on_drop(true);

    let network = if network_allowed {
        ExecutionNetworkReceipt::allowed("profile allows network access")
    } else {
        ExecutionNetworkReceipt::denied("bubblewrap uses --unshare-net")
    };
    command_output_to_receipt(
        ExecutionBackendKind::LinuxBubblewrap,
        capabilities,
        network,
        command,
        &request,
    )
    .await
}

pub(crate) fn linux_bubblewrap_args(
    canonical_cwd: &Path,
    request: &ExecutionRequest,
    network_allowed: bool,
) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("--die-with-parent"),
        OsString::from("--new-session"),
        OsString::from("--unshare-pid"),
    ];
    if !network_allowed {
        args.push(OsString::from("--unshare-net"));
    }
    args.extend([
        OsString::from("--ro-bind"),
        OsString::from("/"),
        OsString::from("/"),
        OsString::from("--tmpfs"),
        OsString::from("/tmp"),
    ]);
    linux_bubblewrap_add_tmpfs_bind_parent(&mut args, canonical_cwd);
    args.extend([
        OsString::from("--bind"),
        canonical_cwd.as_os_str().to_owned(),
        canonical_cwd.as_os_str().to_owned(),
        OsString::from("--proc"),
        OsString::from("/proc"),
        OsString::from("--dev"),
        OsString::from("/dev"),
    ]);
    if let Some(scratch_dir) = request
        .env
        .get(SIGIL_SCRATCH_DIR_ENV)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .and_then(|path| fs::canonicalize(path).ok())
        .filter(|path| !path.starts_with(canonical_cwd))
    {
        linux_bubblewrap_add_tmpfs_bind_parent(&mut args, &scratch_dir);
        args.extend([
            OsString::from("--bind"),
            scratch_dir.as_os_str().to_owned(),
            scratch_dir.as_os_str().to_owned(),
        ]);
    }
    args.extend([
        OsString::from("--chdir"),
        canonical_cwd.as_os_str().to_owned(),
        OsString::from("--"),
    ]);
    args
}

pub(crate) fn linux_bubblewrap_add_tmpfs_bind_parent(args: &mut Vec<OsString>, destination: &Path) {
    if !destination.starts_with("/tmp") {
        return;
    }
    if let Some(parent) = destination.parent() {
        args.extend([OsString::from("--dir"), parent.as_os_str().to_owned()]);
    }
}
