use anyhow::Result;
use sigil_kernel::{
    ExecutionBackend, ExecutionBackendCapabilities, ExecutionBackendKind, ExecutionFuture,
    ExecutionNetworkReceipt, ExecutionReceipt, ExecutionRequest,
};
use tokio::process::Command;

use super::{command_output_to_receipt_with_cancellation, configure_command_environment};

#[derive(Debug, Default)]
pub struct LocalExecutionBackend;

impl ExecutionBackend for LocalExecutionBackend {
    fn kind(&self) -> ExecutionBackendKind {
        ExecutionBackendKind::Local
    }

    fn capabilities(&self) -> ExecutionBackendCapabilities {
        ExecutionBackendCapabilities::default()
    }

    fn planned_network_receipt(&self) -> ExecutionNetworkReceipt {
        ExecutionNetworkReceipt::unknown(
            "local execution backend does not enforce a network policy",
        )
    }

    fn execute(&self, request: ExecutionRequest) -> ExecutionFuture<'_> {
        Box::pin(local_execute(
            self.kind(),
            self.capabilities(),
            request,
            None,
        ))
    }

    fn execute_with_cancellation(
        &self,
        request: ExecutionRequest,
        cancellation: Option<sigil_kernel::RunCancellationHandle>,
    ) -> ExecutionFuture<'_> {
        Box::pin(local_execute(
            self.kind(),
            self.capabilities(),
            request,
            cancellation,
        ))
    }
}

pub(crate) async fn local_execute(
    backend: ExecutionBackendKind,
    capabilities: ExecutionBackendCapabilities,
    request: ExecutionRequest,
    cancellation: Option<sigil_kernel::RunCancellationHandle>,
) -> Result<ExecutionReceipt> {
    let mut command = Command::new(&request.program);
    command
        .args(&request.args)
        .current_dir(&request.cwd)
        .kill_on_drop(true);
    configure_command_environment(&mut command, &request);

    command_output_to_receipt_with_cancellation(
        backend,
        capabilities,
        ExecutionNetworkReceipt::unknown("local backend does not report network enforcement"),
        command,
        &request,
        cancellation,
    )
    .await
}
