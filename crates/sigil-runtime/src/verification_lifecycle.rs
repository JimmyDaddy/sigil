use std::sync::Arc;

use anyhow::{Result, bail};
use sigil_kernel::{
    ExecutionReceipt, ExecutionRequest, RunCancellationHandle, ToolRegistry,
    verification::VerificationExecutionPortV1,
};

use crate::AgentToolBackgroundRuns;

/// Adds host-owned MCP settlement to the existing managed verification execution route.
/// Active child scopes retain their exact generations; idle generations are joined before
/// readiness observes the durable workspace evidence. This grants no new execution authority.
pub fn verification_with_mcp_settlement(
    inner: Arc<dyn VerificationExecutionPortV1>,
    registry: ToolRegistry,
    background_runs: AgentToolBackgroundRuns,
) -> Arc<dyn VerificationExecutionPortV1> {
    Arc::new(McpSettledVerification {
        inner,
        registry,
        background_runs,
    })
}

struct McpSettledVerification {
    inner: Arc<dyn VerificationExecutionPortV1>,
    registry: ToolRegistry,
    background_runs: AgentToolBackgroundRuns,
}

#[async_trait::async_trait]
impl VerificationExecutionPortV1 for McpSettledVerification {
    async fn prepare_verification(&self) -> Result<()> {
        settle_idle_mcp(&self.registry, &self.background_runs).await?;
        self.inner.prepare_verification().await
    }

    async fn execute_check(&self, request: ExecutionRequest) -> Result<ExecutionReceipt> {
        self.inner.execute_check(request).await
    }

    async fn execute_check_with_cancellation(
        &self,
        request: ExecutionRequest,
        cancellation: Option<RunCancellationHandle>,
    ) -> Result<ExecutionReceipt> {
        self.inner
            .execute_check_with_cancellation(request, cancellation)
            .await
    }
}

/// Joins idle extensions while preserving generations used by unfinished child runs.
pub(crate) async fn settle_idle_mcp(
    registry: &ToolRegistry,
    background_runs: &AgentToolBackgroundRuns,
) -> Result<()> {
    let mut failures = Vec::new();
    if let Err(error) = registry
        .quiesce_background_work(&sigil_kernel::ToolBackgroundWorkSettlement::CancelIdle)
        .await
    {
        failures.push(format!("{error:#}"));
    }
    // Freeze the parent's candidates first. A child may activate a new generation while
    // this method runs; it must never be selected from a later, broader parent snapshot.
    let owners = registry.lifecycle_owners_by_namespace(sigil_mcp::MCP_TOOL_LIFECYCLE_NAMESPACE);
    let mut retiring_registry = registry.clone();
    let mut retirements = Vec::new();
    for owner in owners {
        if !background_runs.uses_tool_generation(&owner)? {
            retirements.push(retiring_registry.retire_by_lifecycle_owner(&owner));
        }
    }
    for retirement in retirements {
        if let Err(error) = retirement.dispose_and_quiesce().await {
            failures.push(format!("{error:#}"));
        }
    }
    if !failures.is_empty() {
        bail!("MCP settlement incomplete: {}", failures.join("; "));
    }
    Ok(())
}
