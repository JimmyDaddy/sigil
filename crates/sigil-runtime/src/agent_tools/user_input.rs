use super::*;

/// Result of routing one exact root answer back to its authoritative background child session.
#[derive(Debug, Clone)]
pub struct BackgroundAgentUserInputDecisionResult {
    pub receipt: sigil_kernel::UserInputDecisionReceiptV1,
    pub continuation_started: bool,
}

impl AgentToolRuntime {
    /// Applies a root-attention answer to the authoritative child session and, for submitted
    /// answers, re-registers the suspended background thread under a fresh process-local owner.
    ///
    /// Recovery is deliberately limited to a currently trusted, local, read-only tool surface.
    /// Durable invocation-grant evidence is never promoted back into executable authority.
    pub async fn apply_background_user_input_decision(
        &mut self,
        parent: &mut Session,
        command: sigil_kernel::UserInputDecisionCommandV1,
        parent_options: &AgentRunOptions,
        handler: &mut (dyn EventHandler + Send),
    ) -> Result<BackgroundAgentUserInputDecisionResult> {
        let route =
            sigil_kernel::AgentUserInputRouteProjectionV1::from_session_entries(parent.entries())?
                .route_for_request(&command.identity, &command.request_hash)
                .cloned()
                .context("user input decision does not bind a pending background child route")?;
        if route.status != AgentRouteStatus::Requested {
            bail!("background child user-input route is no longer awaiting an answer");
        }
        if self.background_runs.contains(&route.source_thread_id) {
            bail!("background child continuation is already registered in this process");
        }
        let mut child = build_agent_child_session(parent, &route.child_session_ref)?;
        if child.session_scope_id() != command.identity.session_scope_id.as_str() {
            bail!("background child user input belongs to a different child session");
        }
        sigil_kernel::preview_user_input_decision(&child, &command, unix_time_ms())?;
        let submitted = matches!(
            command.decision,
            sigil_kernel::UserInputDecisionV1::Submitted { .. }
        );

        if !submitted {
            let receipt =
                sigil_kernel::accept_user_input_decision(&mut child, command, unix_time_ms())?;
            let (route_status, thread_status) = match receipt.request.resolution {
                Some(sigil_kernel::UserInputResolutionV1::RunCancelled) => {
                    (AgentRouteStatus::Cancelled, AgentThreadStatus::Cancelled)
                }
                _ => (AgentRouteStatus::Resolved, AgentThreadStatus::Interrupted),
            };
            self.supervisor.update_chat_child_user_input_route(
                parent,
                handler,
                &route,
                receipt.request.clone(),
                route_status,
            )?;
            let status = ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
                thread_id: route.source_thread_id.clone(),
                status: thread_status,
                reason: Some("background child user-input request was not submitted".to_owned()),
                updated_at_ms: Some(unix_time_ms()),
            });
            parent.append_control(status.clone())?;
            handler.handle(RunEvent::Control(status))?;
            self.supervisor
                .release_runtime_thread(&route.source_thread_id);
            return Ok(BackgroundAgentUserInputDecisionResult {
                receipt,
                continuation_started: false,
            });
        }

        let resolved_profile = self.resolve_spawn_profile(&route.profile_id)?;
        let role = resolved_profile.execution_role;
        if !matches!(role, AgentRole::Planner | AgentRole::SubagentRead) {
            bail!("background child input recovery requires a read-only agent role");
        }
        let child_registry =
            build_role_tool_registry(&self.base_registry, &self.root_config, role).into_registry();
        if !tool_registry_is_safe_readonly_for_auto_spawn(&child_registry) {
            bail!("background child input recovery requires a local read-only tool surface");
        }
        let provider = self
            .provider_factory
            .build_provider(&self.root_config, role, &route.profile_id)
            .await
            .context("failed to rebuild background child provider")?;
        let child_agent = Agent::new(provider, child_registry);
        let mut child_options = build_role_run_options(
            &self.root_config,
            parent_options.workspace_root.clone(),
            parent_options.interaction_mode,
            role,
        );
        apply_recovered_readonly_child_constraints(
            &mut child_options,
            parent_options,
            resolved_profile.profile.permission_policy,
        );
        let cancellation_owner = RunCancellationOwner::new();
        let cancellation_handle = cancellation_owner.handle();
        let cancellation_task_guard = cancellation_handle
            .register_task()
            .context("failed to register recovered background child cancellation task")?;

        // All fallible provider, profile, tool-surface, and cancellation preparation happens before
        // the answer becomes durable in the child session.
        let receipt =
            sigil_kernel::accept_user_input_decision(&mut child, command, unix_time_ms())?;
        let physical_attempt_id = sigil_kernel::new_provider_physical_attempt_id();
        let preparation = sigil_kernel::prepare_user_input_continuation(
            &mut child,
            &route.request.identity,
            &route.request.request_hash,
            "agent-background-supervisor",
            &physical_attempt_id,
            unix_time_ms(),
        )?;
        if preparation.continuation.physical_attempt_id != physical_attempt_id {
            bail!("background child continuation is owned by another physical attempt");
        }

        let mut child_thread = self
            .supervisor
            .resume_chat_child_thread(parent, handler, &route, role)?;
        let mailbox_rx = child_thread
            .mailbox_rx
            .take()
            .context("resumed background child mailbox was not created")?;
        let input = self
            .inherit_root_budgets(
                sigil_kernel::AgentRunInput::without_persisted_user_message(Vec::new())
                    .with_logical_run_id(
                        preparation
                            .continuation
                            .continuation_logical_run_id
                            .as_str(),
                    )
                    .with_user_input_continuation_context(
                        route.request.identity.root_logical_run_id.as_str(),
                        route.source_thread_id.clone(),
                    ),
            )
            .with_initial_provider_physical_attempt_id(physical_attempt_id)
            .with_cancellation(cancellation_handle);
        let thread_record = BackgroundChatAgentThreadRecord::from_thread(&child_thread);
        let run_thread = thread_record.clone();
        let child_session_ref = child_thread.child_session_ref.clone();
        let thread_id = child_thread.thread_id.clone();
        let event_sink = self.background_runs.event_sink();
        let (start_tx, start_rx) = tokio::sync::oneshot::channel();
        let handle =
            BackgroundChatAgentTask::spawn(thread_id.clone(), event_sink.clone(), async move {
                let _cancellation_task_guard = cancellation_task_guard;
                start_rx
                    .await
                    .map_err(|_| anyhow!("background child resume gate was cancelled"))?;
                run_background_chat_agent(
                    run_thread,
                    child_agent,
                    child,
                    child_session_ref,
                    input,
                    child_options,
                    mailbox_rx,
                    event_sink,
                )
                .await
            });
        if let Err(error) = self.background_runs.insert(
            thread_id.clone(),
            BackgroundChatAgentHandle {
                thread: thread_record,
                handle,
                cancellation_owner,
            },
        ) {
            drop(start_tx);
            self.supervisor
                .restore_chat_child_blocked_after_resume_failure(
                    parent,
                    handler,
                    &thread_id,
                    &route.route_id,
                    &format!("{error:#}"),
                )?;
            return Err(error);
        }
        if let Err(error) = self.supervisor.update_chat_child_user_input_route(
            parent,
            handler,
            &route,
            receipt.request.clone(),
            AgentRouteStatus::Registered,
        ) {
            drop(start_tx);
            if let Some(background) = self.background_runs.remove_registration(&thread_id) {
                background.handle.abort();
            }
            let _ = self
                .supervisor
                .restore_chat_child_blocked_after_resume_failure(
                    parent,
                    handler,
                    &thread_id,
                    &route.route_id,
                    "failed to commit resumed route ownership",
                );
            return Err(error);
        }
        start_tx
            .send(())
            .map_err(|_| anyhow!("background child resume gate closed before dispatch"))?;
        Ok(BackgroundAgentUserInputDecisionResult {
            receipt,
            continuation_started: true,
        })
    }
}
