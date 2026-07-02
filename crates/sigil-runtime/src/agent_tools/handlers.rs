use super::*;

pub(super) struct ChatAgentApprovalRouteHandler<'a> {
    pub(super) inner: &'a mut (dyn ApprovalHandler + Send),
    pub(super) parent_session: &'a mut Session,
    pub(super) source_thread_id: AgentThreadId,
}

pub(super) struct BackgroundApprovalHandler;

pub(super) struct ChatChildEventHandler<'a> {
    pub(super) inner: &'a mut (dyn EventHandler + Send),
}

pub(super) struct ChatChildThreadGuard {
    pub(super) supervisor: AgentSupervisor,
    pub(super) thread_id: AgentThreadId,
}

impl Drop for ChatChildThreadGuard {
    fn drop(&mut self) {
        self.supervisor.release_runtime_thread(&self.thread_id);
    }
}

impl EventHandler for ChatChildEventHandler<'_> {
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        match event {
            RunEvent::ToolApprovalRequested {
                call,
                spec,
                subjects,
                operation,
                risk,
                subject_zones,
                confirmation,
                snapshot_required,
                preview,
            } => self.inner.handle(RunEvent::ToolApprovalRequested {
                call,
                spec,
                subjects,
                operation,
                risk,
                subject_zones,
                confirmation,
                snapshot_required,
                preview,
            }),
            RunEvent::ToolApprovalResolved {
                call_id,
                approved,
                reason,
            } => self.inner.handle(RunEvent::ToolApprovalResolved {
                call_id,
                approved,
                reason,
            }),
            _ => Ok(()),
        }
    }
}

impl ApprovalHandler for BackgroundApprovalHandler {
    fn approve_tool_call(&mut self, call: &ToolCall, _spec: &ToolSpec) -> Result<ToolApproval> {
        Ok(ToolApproval::Deny {
            reason: format!(
                "background agent cannot request interactive approval for {}",
                call.name
            ),
        })
    }
}

impl ApprovalHandler for ChatAgentApprovalRouteHandler<'_> {
    fn approve_tool_call(&mut self, call: &ToolCall, spec: &ToolSpec) -> Result<ToolApproval> {
        let route_id = agent_route_id_for_call(&self.source_thread_id, &call.id)?;
        self.parent_session
            .append_control(ControlEntry::AgentApprovalRoute(AgentApprovalRouteEntry {
                route_id: route_id.clone(),
                source_thread_id: self.source_thread_id.clone(),
                target_thread_id: None,
                call_id: call.id.clone(),
                tool_name: call.name.clone(),
                status: AgentRouteStatus::Requested,
            }))?;
        let approval = self.inner.approve_tool_call(call, spec)?;
        let status = match approval {
            ToolApproval::Approve
            | ToolApproval::ApproveForSession
            | ToolApproval::ApproveWithArgs { .. } => AgentRouteStatus::Resolved,
            ToolApproval::Deny { .. } => AgentRouteStatus::Rejected,
        };
        self.parent_session
            .append_control(ControlEntry::AgentApprovalRoute(AgentApprovalRouteEntry {
                route_id,
                source_thread_id: self.source_thread_id.clone(),
                target_thread_id: None,
                call_id: call.id.clone(),
                tool_name: call.name.clone(),
                status,
            }))?;
        Ok(approval)
    }
}
