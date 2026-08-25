//! RFC-0071 R71.4: built-in tools consume the managed execution port.
//!
//! This port is deliberately narrower than the legacy `ExecutionBackend`: the runtime owns the
//! authority admission and sandbox launch, while tools only submit the already-approved command
//! request and project its receipt. The legacy adapter remains solely for existing unit fixtures
//! until Terminal and verification move to the same route.

use anyhow::Result;
use async_trait::async_trait;
use sigil_kernel::managed_execution::{ManagedExecutionErrorV1, ManagedProcessHandleV1};
use sigil_kernel::{
    ExecutionBackend, ExecutionBackendCapabilities, ExecutionBackendKind, ExecutionNetworkReceipt,
    ExecutionReceipt, ExecutionRequest, RunCancellationHandle,
};

/// Host-private request for one long-lived terminal process.
///
/// The request is intentionally consumed by the runtime adapter immediately. It is not a
/// kernel contract: the runtime turns these host paths into opaque subject bindings before it
/// calls the managed execution service.
#[derive(Debug, Clone)]
pub struct ManagedTerminalStartRequestV1 {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: std::path::PathBuf,
    pub environment: std::collections::BTreeMap<String, String>,
}

/// Runtime-owned persistent terminal launch port. Built-in terminal lifecycle code only knows
/// this port and the kernel process handle; it never selects an `ExecutionBackend` or spawns a
/// child itself on the production route.
#[async_trait]
pub trait ManagedTerminalExecutionPortV1: Send + Sync {
    async fn start_persistent(
        &self,
        request: ManagedTerminalStartRequestV1,
    ) -> Result<Box<dyn ManagedProcessHandleV1>, ManagedExecutionErrorV1>;
}

#[async_trait]
pub trait ManagedCommandExecutionPortV1: Send + Sync {
    fn kind(&self) -> ExecutionBackendKind;

    fn capabilities(&self) -> ExecutionBackendCapabilities;

    fn planned_network_receipt(&self) -> ExecutionNetworkReceipt;

    async fn execute_with_cancellation(
        &self,
        request: ExecutionRequest,
        cancellation: Option<RunCancellationHandle>,
    ) -> Result<ExecutionReceipt>;
}

pub(crate) struct LegacyBackendCommandExecutionPortV1 {
    pub(crate) backend: std::sync::Arc<dyn ExecutionBackend>,
}

#[async_trait]
impl ManagedCommandExecutionPortV1 for LegacyBackendCommandExecutionPortV1 {
    fn kind(&self) -> ExecutionBackendKind {
        self.backend.kind()
    }

    fn capabilities(&self) -> ExecutionBackendCapabilities {
        self.backend.capabilities()
    }

    fn planned_network_receipt(&self) -> ExecutionNetworkReceipt {
        self.backend.planned_network_receipt()
    }

    async fn execute_with_cancellation(
        &self,
        request: ExecutionRequest,
        cancellation: Option<RunCancellationHandle>,
    ) -> Result<ExecutionReceipt> {
        Ok(self
            .backend
            .execute_with_cancellation(request, cancellation)
            .await?)
    }
}

/// Fail-closed port used by surfaces that have not composed the authority owner yet. It keeps
/// registration and diagnostics available without silently restoring a direct process launcher.
#[derive(Debug, Default)]
pub struct UnavailableManagedCommandExecutionPortV1;

#[async_trait]
impl ManagedCommandExecutionPortV1 for UnavailableManagedCommandExecutionPortV1 {
    fn kind(&self) -> ExecutionBackendKind {
        ExecutionBackendKind::Local
    }

    fn capabilities(&self) -> ExecutionBackendCapabilities {
        ExecutionBackendCapabilities::default()
    }

    fn planned_network_receipt(&self) -> ExecutionNetworkReceipt {
        ExecutionNetworkReceipt::unknown("managed execution authority is not composed")
    }

    async fn execute_with_cancellation(
        &self,
        _request: ExecutionRequest,
        _cancellation: Option<RunCancellationHandle>,
    ) -> Result<ExecutionReceipt> {
        Err(anyhow::anyhow!(
            "managed execution authority is unavailable; refusing direct process fallback"
        ))
    }
}
