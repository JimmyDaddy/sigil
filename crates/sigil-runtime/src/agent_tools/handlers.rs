use super::*;

pub(super) struct ChatAgentApprovalRouteHandler<'a> {
    pub(super) inner: &'a mut (dyn ApprovalHandler + Send),
    pub(super) parent_session: &'a mut Session,
    pub(super) source_thread_id: AgentThreadId,
}

pub(super) struct BackgroundApprovalHandler {
    thread: BackgroundChatAgentThreadRecord,
    source_workspace_id: String,
}

#[derive(Debug, Clone)]
pub(super) struct BackgroundApprovalRequired {
    route: AgentApprovalRouteEntry,
}

impl BackgroundApprovalRequired {
    pub(super) fn route(&self) -> &AgentApprovalRouteEntry {
        &self.route
    }
}

impl std::fmt::Display for BackgroundApprovalRequired {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "background agent {} is blocked waiting for approval route {}",
            self.route.source_thread_id.as_str(),
            self.route.route_id.as_str()
        )
    }
}

impl std::error::Error for BackgroundApprovalRequired {}

impl BackgroundApprovalHandler {
    pub(super) fn new(
        thread: BackgroundChatAgentThreadRecord,
        workspace_root: &Path,
    ) -> Result<Self> {
        Ok(Self {
            thread,
            source_workspace_id: stable_workspace_id(workspace_root)?,
        })
    }
}

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
                approval_identity,
                effects,
                analysis,
                containment,
                safe_summary,
                decision_reasons,
                session_grant_available,
                session_grant_unavailable_reason,
                call,
                spec,
                subjects,
                network_effect,
                local_policy_decision,
                network_policy_decision,
                source_policy_decision,
                operation,
                risk,
                subject_zones,
                confirmation,
                snapshot_required,
                command_permission_matches,
                preview,
            } => self.inner.handle(RunEvent::ToolApprovalRequested {
                approval_identity,
                effects,
                analysis,
                containment,
                safe_summary,
                decision_reasons,
                session_grant_available,
                session_grant_unavailable_reason,
                call,
                spec,
                subjects,
                network_effect,
                local_policy_decision,
                network_policy_decision,
                source_policy_decision,
                operation,
                risk,
                subject_zones,
                confirmation,
                snapshot_required,
                command_permission_matches,
                preview,
            }),
            RunEvent::ToolApprovalResolved {
                call_id,
                approval_request_id,
                approved,
                reason,
            } => self.inner.handle(RunEvent::ToolApprovalResolved {
                call_id,
                approval_request_id,
                approved,
                reason,
            }),
            _ => Ok(()),
        }
    }
}

impl ApprovalHandler for BackgroundApprovalHandler {
    fn approve_tool_call(&mut self, _call: &ToolCall, _spec: &ToolSpec) -> Result<ToolApproval> {
        bail!("background approval requires an exact permission context")
    }

    fn approve_tool_call_with_context(
        &mut self,
        call: &ToolCall,
        _spec: &ToolSpec,
        context: &ToolApprovalContext,
    ) -> Result<ToolApproval> {
        let route = AgentApprovalRouteEntry {
            route_id: agent_route_id_for_call(&self.thread.thread_id, &call.id)?,
            source_thread_id: self.thread.thread_id.clone(),
            target_thread_id: Some(self.thread.parent_thread_id.clone()),
            call_id: call.id.clone(),
            tool_name: call.name.clone(),
            binding: Some(AgentApprovalRouteBinding {
                batch_id: self.thread.batch_id.clone(),
                attempt_id: self.thread.attempt_id.clone(),
                permission_signature: context.permission_signature.clone(),
                policy_fingerprint: context.policy_fingerprint.clone(),
                source_workspace_id: self.source_workspace_id.clone(),
                isolation: self.thread.isolation,
                requested_at_ms: context.requested_at_ms,
                expires_at_ms: context.expires_at_ms,
            }),
            status: AgentRouteStatus::Requested,
        };
        Err(BackgroundApprovalRequired { route }.into())
    }
}

impl ApprovalHandler for ChatAgentApprovalRouteHandler<'_> {
    fn approval_is_explicit_user_action(&self) -> bool {
        self.inner.approval_is_explicit_user_action()
    }

    fn should_present_tool_approval(
        &mut self,
        call: &ToolCall,
        spec: &ToolSpec,
        context: &ToolApprovalContext,
    ) -> Result<bool> {
        self.inner.should_present_tool_approval(call, spec, context)
    }

    fn tool_approval_presentation_failed(
        &mut self,
        call: &ToolCall,
        spec: &ToolSpec,
        context: &ToolApprovalContext,
        reason: &str,
    ) -> Result<()> {
        self.inner
            .tool_approval_presentation_failed(call, spec, context, reason)
    }

    fn approve_tool_call(&mut self, call: &ToolCall, spec: &ToolSpec) -> Result<ToolApproval> {
        let route_id = agent_route_id_for_call(&self.source_thread_id, &call.id)?;
        self.parent_session
            .append_control(ControlEntry::AgentApprovalRoute(AgentApprovalRouteEntry {
                route_id: route_id.clone(),
                source_thread_id: self.source_thread_id.clone(),
                target_thread_id: None,
                call_id: call.id.clone(),
                tool_name: call.name.clone(),
                binding: None,
                status: AgentRouteStatus::Requested,
            }))?;
        let approval = self.inner.approve_tool_call(call, spec)?;
        let status = match approval {
            ToolApproval::Approve
            | ToolApproval::ApproveForSession
            | ToolApproval::ApproveWithArgs { .. } => AgentRouteStatus::Resolved,
            ToolApproval::Deny { .. } => AgentRouteStatus::Rejected,
            ToolApproval::Expired { .. } => AgentRouteStatus::Expired,
            ToolApproval::Cancelled { .. } => AgentRouteStatus::Cancelled,
            ToolApproval::Stale { .. } => AgentRouteStatus::Stale,
        };
        self.parent_session
            .append_control(ControlEntry::AgentApprovalRoute(AgentApprovalRouteEntry {
                route_id,
                source_thread_id: self.source_thread_id.clone(),
                target_thread_id: None,
                call_id: call.id.clone(),
                tool_name: call.name.clone(),
                binding: None,
                status,
            }))?;
        Ok(approval)
    }

    fn approve_tool_call_with_context(
        &mut self,
        call: &ToolCall,
        spec: &ToolSpec,
        context: &ToolApprovalContext,
    ) -> Result<ToolApproval> {
        let route_id = agent_route_id_for_call(&self.source_thread_id, &call.id)?;
        self.parent_session
            .append_control(ControlEntry::AgentApprovalRoute(AgentApprovalRouteEntry {
                route_id: route_id.clone(),
                source_thread_id: self.source_thread_id.clone(),
                target_thread_id: None,
                call_id: call.id.clone(),
                tool_name: call.name.clone(),
                binding: None,
                status: AgentRouteStatus::Requested,
            }))?;
        let approval = self
            .inner
            .approve_tool_call_with_context(call, spec, context)?;
        let status = match approval {
            ToolApproval::Approve
            | ToolApproval::ApproveForSession
            | ToolApproval::ApproveWithArgs { .. } => AgentRouteStatus::Resolved,
            ToolApproval::Deny { .. } => AgentRouteStatus::Rejected,
            ToolApproval::Expired { .. } => AgentRouteStatus::Expired,
            ToolApproval::Cancelled { .. } => AgentRouteStatus::Cancelled,
            ToolApproval::Stale { .. } => AgentRouteStatus::Stale,
        };
        self.parent_session
            .append_control(ControlEntry::AgentApprovalRoute(AgentApprovalRouteEntry {
                route_id,
                source_thread_id: self.source_thread_id.clone(),
                target_thread_id: None,
                call_id: call.id.clone(),
                tool_name: call.name.clone(),
                binding: None,
                status,
            }))?;
        Ok(approval)
    }
}
