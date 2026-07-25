use super::*;

impl AgentToolRuntime {
    pub(super) async fn request_agent_delegation(
        &mut self,
        session: &mut Session,
        call: &ToolCall,
        args: &Value,
        options: &AgentRunOptions,
        handler: &mut (dyn EventHandler + Send),
        approval_handler: &mut (dyn ApprovalHandler + Send),
    ) -> ToolResult {
        let parsed = match RequestAgentDelegationArgs::parse(args) {
            Ok(parsed) => parsed,
            Err(error) => {
                return delegation_request_error(
                    call,
                    ToolErrorKind::InvalidInput,
                    format!("{error:#}"),
                );
            }
        };
        if self.root_config.task.multi_agent_mode == MultiAgentMode::None {
            return delegation_request_error(
                call,
                ToolErrorKind::PermissionDenied,
                "agent delegation is disabled by [task].multi_agent_mode=none".to_owned(),
            );
        }
        if !self
            .agent_tool_authorization
            .as_ref()
            .is_some_and(|authorization| authorization.matches_explicitly_approved(call))
        {
            return delegation_request_error(
                call,
                ToolErrorKind::ApprovalRequired,
                "request_agent_delegation requires an explicit user confirmation for this exact proposal"
                    .to_owned(),
            );
        }

        let proposed_context = match self.model_delegation_run_context(session) {
            Ok(context) => context,
            Err(error) => {
                return delegation_request_error(
                    call,
                    ToolErrorKind::PermissionDenied,
                    format!("{error:#}"),
                );
            }
        };
        if !matches!(
            &proposed_context.source,
            AgentInvocationGrantSource::Conversation { .. }
        ) || proposed_context.authority != DelegationAuthority::ModelProactive
        {
            return delegation_request_error(
                call,
                ToolErrorKind::PermissionDenied,
                "request_agent_delegation is only available to an ordinary conversation proposal"
                    .to_owned(),
            );
        }

        let previous_context = self
            .delegation_run_context
            .replace(AgentDelegationRunContext {
                source: proposed_context.source,
                authority: DelegationAuthority::UserExplicit,
            });
        let result = if let Some(single_args) = parsed.single_spawn_args() {
            self.spawn_agent(
                session,
                call,
                &single_args,
                options,
                handler,
                approval_handler,
            )
            .await
        } else {
            let batch_args = parsed.batch_spawn_args();
            self.spawn_agents(session, call, &batch_args, options, handler)
                .await
        };
        self.delegation_run_context = previous_context;
        result
    }
}

fn delegation_request_error(call: &ToolCall, kind: ToolErrorKind, message: String) -> ToolResult {
    ToolResult::error(call.id.clone(), call.name.clone(), kind, message.clone()).with_error_details(
        false,
        json!({
            "error": "agent_delegation_proposal_rejected",
            "message": message,
            "provider_started": false,
            "child_started": false,
        }),
    )
}
