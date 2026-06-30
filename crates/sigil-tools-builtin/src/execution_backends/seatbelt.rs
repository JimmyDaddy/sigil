use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use sigil_kernel::{
    ExecutionBackend, ExecutionBackendCapabilities, ExecutionBackendKind, ExecutionFuture,
    ExecutionNetworkReceipt, ExecutionReceipt, ExecutionRequest,
};
use tokio::process::Command;

use super::command_output_to_receipt;

#[derive(Debug, Clone)]
pub struct MacosSeatbeltExecutionBackend {
    sandbox_exec: PathBuf,
    network_allowed: bool,
}

impl Default for MacosSeatbeltExecutionBackend {
    fn default() -> Self {
        Self {
            sandbox_exec: PathBuf::from("/usr/bin/sandbox-exec"),
            network_allowed: false,
        }
    }
}

impl MacosSeatbeltExecutionBackend {
    #[must_use]
    pub fn new(sandbox_exec: PathBuf) -> Self {
        Self {
            sandbox_exec,
            network_allowed: false,
        }
    }

    #[must_use]
    pub fn with_network_allowed(mut self, network_allowed: bool) -> Self {
        self.network_allowed = network_allowed;
        self
    }

    #[must_use]
    pub fn is_available(&self) -> bool {
        cfg!(target_os = "macos") && self.sandbox_exec.is_file()
    }
}

impl ExecutionBackend for MacosSeatbeltExecutionBackend {
    fn kind(&self) -> ExecutionBackendKind {
        ExecutionBackendKind::MacosSeatbelt
    }

    fn capabilities(&self) -> ExecutionBackendCapabilities {
        ExecutionBackendCapabilities {
            filesystem_isolation: true,
            network_isolation: false,
            process_isolation: true,
            resource_limits: false,
            persistent_pty: false,
            workspace_snapshot: false,
        }
    }

    fn execute(&self, request: ExecutionRequest) -> ExecutionFuture<'_> {
        let sandbox_exec = self.sandbox_exec.clone();
        let network_allowed = self.network_allowed;
        Box::pin(
            async move { macos_seatbelt_execute(sandbox_exec, network_allowed, request).await },
        )
    }
}

pub(crate) fn ensure_macos_seatbelt_available(
    backend: &MacosSeatbeltExecutionBackend,
) -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("macos_seatbelt execution backend is only available on macOS");
    }
    if !backend.is_available() {
        bail!(
            "macos_seatbelt execution backend requires {}",
            backend.sandbox_exec.display()
        );
    }
    Ok(())
}

pub(crate) async fn macos_seatbelt_execute(
    sandbox_exec: PathBuf,
    network_allowed: bool,
    request: ExecutionRequest,
) -> Result<ExecutionReceipt> {
    if !cfg!(target_os = "macos") {
        bail!("macos_seatbelt execution backend is only available on macOS");
    }
    if !sandbox_exec.is_file() {
        bail!(
            "macos_seatbelt execution backend requires {}",
            sandbox_exec.display()
        );
    }

    let canonical_cwd = fs::canonicalize(&request.cwd)
        .with_context(|| format!("failed to canonicalize cwd {}", request.cwd.display()))?;
    let profile = macos_seatbelt_workspace_write_profile(&canonical_cwd);
    let capabilities = MacosSeatbeltExecutionBackend::default().capabilities();

    let mut command = Command::new(&sandbox_exec);
    command
        .arg("-p")
        .arg(profile)
        .arg(&request.program)
        .args(&request.args)
        .current_dir(&canonical_cwd)
        .envs(&request.env)
        .kill_on_drop(true);

    let network = if network_allowed {
        ExecutionNetworkReceipt::allowed("profile allows network access")
    } else {
        ExecutionNetworkReceipt::unsupported(
            "macos_seatbelt backend does not enforce network denial",
        )
    };
    command_output_to_receipt(
        ExecutionBackendKind::MacosSeatbelt,
        capabilities,
        network,
        command,
        &request,
    )
    .await
}

pub(crate) fn macos_seatbelt_workspace_write_profile(workspace_root: &Path) -> String {
    let workspace = macos_seatbelt_string_literal(&workspace_root.to_string_lossy());
    format!(
        r#"(version 1)
(deny default)
(allow process*)
(allow file-read*)
(allow file-write* (subpath "{}"))
"#,
        workspace
    )
}

pub(crate) fn macos_seatbelt_string_literal(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}
