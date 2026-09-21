use super::*;

/// Runtime-neutral seam for the one model-owned Task root loop.
///
/// The kernel owns admission, durable attempt state, and terminal authority. The runtime only
/// materializes the provider/tool session that executes the already-admitted direct objective.
#[async_trait]
pub trait TaskChildSessionRunner: Send + Sync {
    async fn run_direct_execution_session<H, A>(
        &self,
        parent_session: &mut Session,
        request: TaskDirectExecutionSessionRunRequest,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<TaskDirectExecutionSessionRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send;
}
