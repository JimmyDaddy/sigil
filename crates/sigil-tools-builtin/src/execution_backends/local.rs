use anyhow::Result;
use sigil_kernel::{
    ExecutionBackend, ExecutionBackendCapabilities, ExecutionBackendKind, ExecutionFuture,
    ExecutionNetworkReceipt, ExecutionReceipt, ExecutionRequest,
};
use tokio::process::Command;

use super::command_output_to_receipt;

#[derive(Debug, Default)]
pub struct LocalExecutionBackend;

impl ExecutionBackend for LocalExecutionBackend {
    fn kind(&self) -> ExecutionBackendKind {
        ExecutionBackendKind::Local
    }

    fn capabilities(&self) -> ExecutionBackendCapabilities {
        ExecutionBackendCapabilities::default()
    }

    fn execute(&self, request: ExecutionRequest) -> ExecutionFuture<'_> {
        Box::pin(local_execute(self.kind(), self.capabilities(), request))
    }
}

pub(crate) async fn local_execute(
    backend: ExecutionBackendKind,
    capabilities: ExecutionBackendCapabilities,
    request: ExecutionRequest,
) -> Result<ExecutionReceipt> {
    let mut command = Command::new(&request.program);
    command
        .args(&request.args)
        .current_dir(&request.cwd)
        .envs(&request.env)
        .kill_on_drop(true);

    command_output_to_receipt(
        backend,
        capabilities,
        ExecutionNetworkReceipt::unknown("local backend does not report network enforcement"),
        command,
        &request,
    )
    .await
}
