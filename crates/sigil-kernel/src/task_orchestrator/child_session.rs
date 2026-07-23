use super::*;

/// Runtime-neutral contract for launching task child sessions.
///
/// The kernel owns task control-plane semantics, but runtime implementations own concrete child
/// session creation, profile snapshots, provider/tool assembly, and route-aware child lifecycle.
#[async_trait]
pub trait TaskChildSessionRunner: Send + Sync {
    /// Runs the task planner in an isolated transcript and returns its accepted plan artifact.
    async fn run_planner_session<H, A>(
        &self,
        _parent_session: &mut Session,
        _request: TaskPlannerSessionRunRequest,
        _handler: &mut H,
        _approval_handler: &mut A,
    ) -> Result<TaskPlannerSessionRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        bail!("task child session runner does not support isolated planner sessions")
    }

    /// Runs one task child session and returns its bounded terminal output.
    ///
    /// # Errors
    ///
    /// Returns an error when child session creation, control-log append, approval routing, or the
    /// child agent run fails before a terminal result can be recorded.
    async fn run_child_session<H, A>(
        &self,
        parent_session: &mut Session,
        request: TaskChildSessionRunRequest,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<TaskChildSessionRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send;

    /// Runs one ready read-only batch and returns member outcomes in request order.
    ///
    /// The default preserves compatibility by executing members sequentially. Runtime
    /// implementations that can detach child execution from the parent session should override
    /// this method and keep parent-session mutation in prepare/commit phases around the concurrent
    /// child futures.
    async fn run_child_session_batch<H, A>(
        &self,
        parent_session: &mut Session,
        requests: Vec<TaskChildSessionRunRequest>,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<Vec<Result<TaskChildSessionRunOutput>>>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        let mut outputs = Vec::with_capacity(requests.len());
        for request in requests {
            outputs.push(
                self.run_child_session(parent_session, request, handler, approval_handler)
                    .await,
            );
        }
        Ok(outputs)
    }

    /// Runs final synthesis in an isolated read-only transcript.
    async fn run_synthesis_session<H, A>(
        &self,
        _parent_session: &mut Session,
        _request: TaskSynthesisSessionRunRequest,
        _handler: &mut H,
        _approval_handler: &mut A,
    ) -> Result<TaskSynthesisSessionRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        bail!("task child session runner does not support isolated synthesis sessions")
    }
}
