use super::surface::SpawnIsolation;
use super::*;

impl AgentToolRuntime {
    pub(super) async fn spawn_agent(
        &mut self,
        session: &mut Session,
        call: &ToolCall,
        args: &Value,
        options: &sigil_kernel::AgentRunOptions,
        handler: &mut (dyn EventHandler + Send),
        approval_handler: &mut (dyn ApprovalHandler + Send),
    ) -> ToolResult {
        let parsed = match SpawnAgentArgs::parse(args) {
            Ok(parsed) => parsed,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::InvalidInput,
                    error.to_string(),
                );
            }
        };
        let resolved_profile = match self.resolve_spawn_profile(&parsed.profile_id) {
            Ok(profile) => profile,
            Err(error) => {
                return agent_spawn_denied_tool_result(call, format!("{error:#}"));
            }
        };
        let role = resolved_profile.execution_role;
        let isolation = parsed
            .isolation
            .unwrap_or_else(|| default_spawn_isolation(role, &resolved_profile));
        if isolation == SpawnIsolation::Worktree
            && (role != AgentRole::SubagentWrite
                || !profile_uses_changeset_only_write(role, &resolved_profile)
                || !self.root_config.task.allow_write_subagents)
        {
            return agent_spawn_denied_tool_result(
                call,
                "worktree isolation requires the trusted writable worker profile".to_owned(),
            );
        }
        if isolation == SpawnIsolation::ChangesetOnly
            && !profile_uses_changeset_only_write(role, &resolved_profile)
        {
            return agent_spawn_denied_tool_result(
                call,
                "changeset_only isolation requires a trusted writable worker profile".to_owned(),
            );
        }
        let changeset_only_write = isolation == SpawnIsolation::ChangesetOnly;
        let worktree_write = isolation == SpawnIsolation::Worktree;
        let isolated_write = changeset_only_write || worktree_write;
        // The built-in worker profile intentionally narrows its default changeset-only surface.
        // An explicit model-selected worktree is a separate, trusted write contract, so the role
        // registry may expose its writable tools while still keeping the profile's permission and
        // root policy bounds authoritative.
        let profile_tool_scope = if isolation == SpawnIsolation::Worktree {
            sigil_kernel::ToolRegistryScope {
                allow_all: true,
                ..sigil_kernel::ToolRegistryScope::default()
            }
        } else {
            resolved_profile.profile.tool_scope.clone()
        };
        let child_registry = child_tool_registry_for_profile(
            &self.base_registry,
            &self.root_config,
            role,
            isolation,
            profile_tool_scope,
        );
        #[cfg(test)]
        self.ensure_test_delegation_runtime_binding();
        let delegation_context = match self.model_delegation_run_context(session) {
            Ok(context) => context,
            Err(error) => {
                return agent_spawn_denied_tool_result(call, format!("{error:#}"));
            }
        };
        let authority = delegation_context.authority.clone();
        let invocation_source = invocation_source_for_authority(&authority);
        let effective_multi_agent_mode = match self.enforce_effective_multi_agent_mode(session) {
            Ok(mode) => mode,
            Err(error) => {
                return agent_spawn_denied_tool_result(call, format!("{error:#}"));
            }
        };
        if let Err(error) = admit_model_agent_spawn(
            effective_multi_agent_mode,
            &authority,
            &resolved_profile,
            &child_registry,
        ) {
            return agent_spawn_denied_tool_result(call, format!("{error:#}"));
        }
        let thread_id = match chat_agent_thread_id_for_call(&call.id, &parsed.profile_id) {
            Ok(thread_id) => thread_id,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::InvalidInput,
                    error.to_string(),
                );
            }
        };
        let root_logical_run_id = match self.root_logical_run_id.clone() {
            Some(root_logical_run_id) => root_logical_run_id,
            None => {
                return agent_spawn_denied_tool_result(
                    call,
                    "agent spawn is missing the root logical-run identity".to_owned(),
                );
            }
        };
        let root_cancellation = match self.run_cancellation.clone() {
            Some(root_cancellation) => root_cancellation,
            None => {
                return agent_spawn_denied_tool_result(
                    call,
                    "agent spawn is missing the root cancellation scope".to_owned(),
                );
            }
        };
        let mut background_cancellation_owner =
            matches!(parsed.mode, AgentInvocationMode::Background).then(RunCancellationOwner::new);
        let invocation_cancellation = background_cancellation_owner
            .as_ref()
            .map(RunCancellationOwner::handle)
            .unwrap_or_else(|| root_cancellation.clone());
        let isolation = match isolation {
            SpawnIsolation::ChangesetOnly => TaskIsolationMode::ChangesetOnly,
            SpawnIsolation::SharedReadOnly => TaskIsolationMode::SharedReadOnly,
            SpawnIsolation::Worktree => TaskIsolationMode::Worktree,
        };
        let grant = match mint_agent_invocation_grant(
            delegation_context.clone(),
            &root_logical_run_id,
            &invocation_cancellation,
            parsed.profile_id.clone(),
            role,
            isolation,
            &child_registry,
            options,
            unix_time_ms(),
        )
        .and_then(|grant| {
            revalidate_agent_invocation_grant(
                &grant,
                &delegation_context,
                &root_logical_run_id,
                &invocation_cancellation,
                &parsed.profile_id,
                role,
                isolation,
                &child_registry,
                &options.workspace_root,
                unix_time_ms(),
            )?;
            Ok(grant)
        }) {
            Ok(grant) => grant,
            Err(error) => {
                return agent_spawn_denied_tool_result(call, format!("{error:#}"));
            }
        };
        let delegation_admission = match delegation_admission_entry(
            &grant,
            thread_id.clone(),
            parsed.profile_id.clone(),
            parsed.mode,
            invocation_source,
            &parsed.objective,
        ) {
            Ok(admission) => admission,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    error.to_string(),
                );
            }
        };
        if let Some(warning) = spawn_scope_overlap_warning(session, &parsed) {
            let _ = handler.handle(RunEvent::Notice(warning));
        }
        let safe_detachable_registry =
            tool_registry_is_safe_readonly_for_auto_spawn(&child_registry);
        let child_provider = match self
            .provider_factory
            .build_provider(&self.root_config, role, &parsed.profile_id)
            .await
        {
            Ok(provider) => provider,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    format!("failed to build child agent provider: {error:#}"),
                );
            }
        };
        let child_capabilities = child_provider.capabilities();
        let parent_session_ref = match parent_session_ref(session) {
            Ok(reference) => reference,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    error.to_string(),
                );
            }
        };
        let child_session_ref = match agent_child_session_ref(&thread_id) {
            Ok(reference) => reference,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    error.to_string(),
                );
            }
        };
        let budget_scope_id = match chat_budget_scope_id(&call.id) {
            Ok(task_id) => task_id,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    error.to_string(),
                );
            }
        };
        let mut chat_worktree = None;
        let mut child_workspace_root = options.workspace_root.clone();
        if worktree_write {
            match prepare_chat_worktree(session, handler, &thread_id, &options.workspace_root).await
            {
                Ok(materialized) => {
                    child_workspace_root = materialized.workspace_root().to_path_buf();
                    chat_worktree = Some(materialized);
                }
                Err(error) => {
                    return worktree_preparation_unavailable_tool_result(call, &error);
                }
            }
        }
        let mut child_thread = match self.supervisor.begin_chat_child_thread(
            session,
            handler,
            crate::AgentChatChildStart {
                call_id: call.id.clone(),
                budget_scope_id: budget_scope_id.clone(),
                parent_thread_id: match AgentThreadId::new(MAIN_THREAD_ID) {
                    Ok(thread_id) => thread_id,
                    Err(error) => {
                        return ToolResult::error(
                            call.id.clone(),
                            call.name.clone(),
                            ToolErrorKind::Internal,
                            error.to_string(),
                        );
                    }
                },
                parent_depth: 0,
                batch_id: None,
                batch_member_key: None,
                parent_session_ref,
                profile_id: parsed.profile_id.clone(),
                role,
                child_session_ref: child_session_ref.clone(),
                objective: parsed.objective.clone(),
                prompt: parsed.prompt.clone(),
                workspace_root: child_workspace_root.clone(),
                provider_capabilities: child_capabilities,
                invocation_mode: parsed.mode,
                invocation_source,
                invocation_grant: grant.clone(),
                delegation_admission,
                display_name_hint: parsed.display_name_hint.clone(),
            },
        ) {
            Ok(thread) => thread,
            Err(error) => {
                if let Some(worktree) = chat_worktree.take() {
                    let _ = cleanup_chat_worktree(session, handler, worktree).await;
                }
                if let Some(denial) = error
                    .downcast_ref::<crate::agent_supervisor::AgentReservationError>()
                    .and_then(crate::agent_supervisor::AgentReservationError::budget_denial)
                {
                    return agent_budget_denied_tool_result(call, denial);
                }
                return agent_spawn_denied_tool_result(call, format!("{error:#}"));
            }
        };

        let mut child_session = match build_agent_child_session(session, &child_session_ref) {
            Ok(session) => session,
            Err(error) => {
                let _ = self.supervisor.record_chat_child_failure(
                    session,
                    handler,
                    &child_thread,
                    format!("{error:#}"),
                );
                if let Some(worktree) = chat_worktree.take() {
                    let _ = cleanup_chat_worktree(session, handler, worktree).await;
                }
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    error.to_string(),
                );
            }
        };
        let child_agent =
            match crate::configured_agent(&self.root_config, child_provider, child_registry) {
                Ok(agent) => agent,
                Err(error) => {
                    if let Some(worktree) = chat_worktree.take() {
                        let _ = cleanup_chat_worktree(session, handler, worktree).await;
                    }
                    return ToolResult::error(
                        call.id.clone(),
                        call.name.clone(),
                        ToolErrorKind::Internal,
                        format!("failed to configure child agent recovery: {error:#}"),
                    );
                }
            };
        let mut child_messages = Vec::new();
        if let Some(system_prompt) = agent_profile_system_prompt(&resolved_profile) {
            child_messages.push(ModelMessage::system(system_prompt));
        }
        if changeset_only_write {
            child_messages.push(ModelMessage::system(
                changeset_only_child_contract_prompt().to_owned(),
            ));
        }
        child_messages.push(ModelMessage::user(parsed.prompt.clone()));
        let child_input = self
            .inherit_root_budgets(sigil_kernel::AgentRunInput::without_persisted_user_message(
                child_messages,
            ))
            .with_agent_invocation_grant(grant.clone());
        let child_input =
            if matches!(parsed.mode, AgentInvocationMode::Background) && safe_detachable_registry {
                child_input.with_user_input_continuation_context(
                    root_logical_run_id.clone(),
                    thread_id.clone(),
                )
            } else {
                child_input
            };
        let mut child_options = build_role_run_options(
            &self.root_config,
            child_workspace_root.clone(),
            options.interaction_mode,
            role,
        );
        apply_child_permission_constraints(
            &mut child_options,
            options,
            role,
            resolved_profile.profile.permission_policy.clone(),
            &grant,
        );

        let changeset_only_base_snapshot_id = match changeset_only_write {
            true => match capture_chat_changeset_only_parent_snapshot_id(
                session,
                &child_thread.thread_id,
                &options.workspace_root,
                "base",
            ) {
                Ok(snapshot_id) => Some(snapshot_id),
                Err(error) => {
                    let _ = self.supervisor.record_chat_child_failure(
                        session,
                        handler,
                        &child_thread,
                        format!("{error:#}"),
                    );
                    if let Some(worktree) = chat_worktree.take() {
                        let _ = cleanup_chat_worktree(session, handler, worktree).await;
                    }
                    return ToolResult::error(
                        call.id.clone(),
                        call.name.clone(),
                        ToolErrorKind::Internal,
                        error.to_string(),
                    );
                }
            },
            false => None,
        };

        if matches!(parsed.mode, AgentInvocationMode::Background) {
            let Some(mailbox_rx) = child_thread.mailbox_rx.take() else {
                let _ = self.supervisor.record_chat_child_failure(
                    session,
                    handler,
                    &child_thread,
                    "background agent mailbox was not created".to_owned(),
                );
                if let Some(worktree) = chat_worktree.take() {
                    let _ = cleanup_chat_worktree(session, handler, worktree).await;
                }
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    "background agent mailbox was not created",
                );
            };
            let write_owner =
                if let Some(base_snapshot_id) = changeset_only_base_snapshot_id.as_ref() {
                    Some(BackgroundChatAgentWriteOwner::ChangesetOnly {
                        base_snapshot_id: base_snapshot_id.clone(),
                        workspace_root: options.workspace_root.clone(),
                    })
                } else if let Some(worktree) = chat_worktree.take() {
                    Some(BackgroundChatAgentWriteOwner::Worktree {
                        worktree: Box::new(worktree),
                        workspace_root: options.workspace_root.clone(),
                        objective: parsed.objective.clone(),
                    })
                } else {
                    None
                };
            let thread_id = child_thread.thread_id.clone();
            let cancellation_owner = background_cancellation_owner
                .take()
                .expect("background mode creates its cancellation owner before grant minting");
            let cancellation_handle = cancellation_owner.handle();
            let cancellation_task_guard = cancellation_handle
                .register_task()
                .expect("new background cancellation owner must admit its first task");
            let child_input = child_input.with_cancellation(cancellation_handle);
            let thread_record = BackgroundChatAgentThreadRecord::from_thread(&child_thread);
            let run_thread = thread_record.clone();
            let child_session_ref = child_thread.child_session_ref.clone();
            let event_sink = self.background_runs.event_sink();
            let (start_tx, start_rx) = tokio::sync::oneshot::channel();
            let handle =
                BackgroundChatAgentTask::spawn(thread_id.clone(), event_sink.clone(), async move {
                    let _cancellation_task_guard = cancellation_task_guard;
                    start_rx
                        .await
                        .map_err(|_| anyhow!("background agent start gate was cancelled"))?;
                    run_background_chat_agent(
                        run_thread,
                        child_agent,
                        child_session,
                        child_session_ref,
                        child_input,
                        child_options,
                        mailbox_rx,
                        isolated_write,
                        event_sink,
                    )
                    .await
                });
            if let Err(error) = self.background_runs.insert(
                thread_id.clone(),
                BackgroundChatAgentHandle {
                    thread: thread_record,
                    handle,
                    collection_supervisor: self
                        .supervisor
                        .clone()
                        .with_background_runs(AgentToolBackgroundRuns::default()),
                    cancellation_owner,
                    write_owner,
                },
            ) {
                drop(start_tx);
                let _ = self.supervisor.record_chat_child_failure(
                    session,
                    handler,
                    &child_thread,
                    format!("{error:#}"),
                );
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    error.to_string(),
                );
            }
            let _ = start_tx.send(());
            let projection = session.agent_thread_state_projection();
            if let Some(thread) = projection.threads.get(&thread_id) {
                return agent_status_tool_result(session, call, thread);
            }
            return ToolResult::ok(
                call.id.clone(),
                call.name.clone(),
                format!("agent thread {} is running", thread_id.as_str()),
                ToolResultMeta {
                    details: json!({
                        "thread_id": thread_id.as_str(),
                        "status": "running",
                        "retry_after_ms": WAIT_AGENT_RUNNING_RETRY_AFTER_MS,
                    }),
                    ..ToolResultMeta::default()
                },
            );
        }

        if self.join_batch_eligible
            && self.run_cancellation.is_some()
            && !isolated_write
            && matches!(parsed.mode, AgentInvocationMode::JoinBeforeFinal)
            && safe_detachable_registry
        {
            return self.start_joined_chat_child(
                session,
                call,
                child_thread,
                child_agent,
                child_session,
                child_input,
                child_options,
                handler,
                None,
            );
        }

        if self.run_cancellation.is_none()
            && !isolated_write
            && matches!(parsed.mode, AgentInvocationMode::JoinBeforeFinal)
            && safe_detachable_registry
        {
            return self
                .run_detachable_chat_child(
                    session,
                    call,
                    child_thread,
                    child_agent,
                    child_session,
                    child_input,
                    child_options,
                    budget_scope_id,
                    handler,
                )
                .await;
        }

        let child_input = self
            .run_cancellation
            .as_ref()
            .map_or(child_input.clone(), |handle| {
                child_input.with_child_cancellation(handle.clone())
            });

        let _thread_guard = ChatChildThreadGuard {
            supervisor: self.supervisor.clone(),
            thread_id: child_thread.thread_id.clone(),
        };
        let output = {
            let mut child_handler = ChatChildEventHandler { inner: handler };
            let mut route_handler = ChatAgentApprovalRouteHandler {
                inner: approval_handler,
                parent_session: session,
                source_thread_id: child_thread.thread_id.clone(),
            };
            child_agent
                .run_with_approval_input(
                    &mut child_session,
                    child_input,
                    child_options,
                    &mut child_handler,
                    &mut route_handler,
                )
                .await
        };
        let output = match output {
            Ok(output) => output,
            Err(error) => {
                let _ = self.supervisor.record_chat_child_failure(
                    session,
                    handler,
                    &child_thread,
                    format!("{error:#}"),
                );
                if let Some(worktree) = chat_worktree.take() {
                    let _ = cleanup_chat_worktree(session, handler, worktree).await;
                }
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    format!("child agent failed: {error:#}"),
                );
            }
        };
        let materialized = match materialize_child_agent_final_answer(
            &mut child_session,
            &child_thread.child_session_ref,
            &child_thread.thread_id,
            &output.result,
        )
        .await
        {
            Ok(materialized) => materialized,
            Err(error) => {
                let _ = self.supervisor.record_chat_child_failure(
                    session,
                    handler,
                    &child_thread,
                    format!("{error:#}"),
                );
                if let Some(worktree) = chat_worktree.take() {
                    let _ = cleanup_chat_worktree(session, handler, worktree).await;
                }
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    error.to_string(),
                );
            }
        };
        let outcome = output.outcome;
        let worktree_controls = if let Some(worktree) = chat_worktree.as_ref() {
            match prepare_chat_worktree_child_controls(
                session,
                &child_thread.thread_id,
                worktree,
                &outcome,
                &options.workspace_root,
                &parsed.objective,
            )
            .await
            {
                Ok(controls) => controls.map(PreparedChatIsolatedChildControls::Worktree),
                Err(error) => {
                    let _ = self.supervisor.record_chat_child_failure(
                        session,
                        handler,
                        &child_thread,
                        format!("{error:#}"),
                    );
                    if let Some(worktree) = chat_worktree.take() {
                        let _ = cleanup_chat_worktree(session, handler, worktree).await;
                    }
                    return ToolResult::error(
                        call.id.clone(),
                        call.name.clone(),
                        ToolErrorKind::InvalidInput,
                        format!("worktree child output could not be captured: {error:#}"),
                    );
                }
            }
        } else {
            None
        };
        let changeset_only_controls = if let Some(base_snapshot_id) =
            changeset_only_base_snapshot_id
        {
            match prepare_chat_changeset_only_child_controls(
                session,
                &child_thread.thread_id,
                &base_snapshot_id,
                &materialized.final_text,
                &outcome,
                &options.workspace_root,
            ) {
                Ok(controls) => Some(PreparedChatIsolatedChildControls::ChangesetOnly(controls)),
                Err(error) => {
                    let _ = self.supervisor.record_chat_child_failure(
                        session,
                        handler,
                        &child_thread,
                        format!("{error:#}"),
                    );
                    return ToolResult::error(
                        call.id.clone(),
                        call.name.clone(),
                        ToolErrorKind::InvalidInput,
                        format!("changeset-only child output was invalid: {error:#}"),
                    );
                }
            }
        } else {
            None
        };
        let usage = usage_summary_from_stats(child_session.stats());
        let budget_warning = self
            .supervisor
            .validate_usage_budget(&budget_scope_id, &usage)
            .err()
            .map(|error| format!("{error:#}"));
        let status = child_status_from_outcome(&materialized.final_text, &outcome);
        if let Err(error) = self.supervisor.record_chat_child_result(
            session,
            handler,
            &child_thread,
            status,
            &materialized,
            &outcome,
            Some(usage),
        ) {
            if let Some(worktree) = chat_worktree.take() {
                let _ = cleanup_chat_worktree(session, handler, worktree).await;
            }
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::Internal,
                error.to_string(),
            );
        }
        if let Some(controls) = changeset_only_controls
            && let Err(error) =
                append_prepared_chat_isolated_child_controls(session, handler, controls)
        {
            if let Some(worktree) = chat_worktree.take() {
                let _ = cleanup_chat_worktree(session, handler, worktree).await;
            }
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::Internal,
                error.to_string(),
            );
        }
        if let Some(controls) = worktree_controls
            && let Err(error) =
                append_prepared_chat_isolated_child_controls(session, handler, controls)
        {
            if let Some(worktree) = chat_worktree.take() {
                let _ = cleanup_chat_worktree(session, handler, worktree).await;
            }
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::Internal,
                error.to_string(),
            );
        }
        if let Some(worktree) = chat_worktree.take()
            && let Err(error) = cleanup_chat_worktree(session, handler, worktree).await
        {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::Internal,
                error.to_string(),
            );
        }
        if let Some(warning) = budget_warning {
            let _ = handler.handle(RunEvent::Notice(format!(
                "agent budget warning after child completion: {warning}"
            )));
        }
        let projection = session.agent_thread_state_projection();
        let thread = projection.threads.get(&child_thread.thread_id);
        let display_name = thread.and_then(|thread| thread.display_name.as_deref());
        let result = thread.and_then(|thread| thread.result.clone());
        agent_result_tool_result(
            call,
            &child_thread.thread_id,
            display_name,
            result.as_ref(),
            DEFAULT_RESULT_SUMMARY_LIMIT,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn start_joined_chat_child(
        &mut self,
        session: &mut Session,
        call: &ToolCall,
        child_thread: crate::AgentChatChildThread,
        child_agent: Agent<Box<dyn Provider>>,
        child_session: Session,
        child_input: sigil_kernel::AgentRunInput,
        child_options: sigil_kernel::AgentRunOptions,
        handler: &mut (dyn EventHandler + Send),
        batch_member: Option<AgentBatchMemberContext>,
    ) -> ToolResult {
        let Some(root_cancellation) = self.run_cancellation.clone() else {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::Internal,
                "host join requires the current root cancellation scope",
            );
        };
        if root_cancellation.is_cancel_requested() {
            let reason = "root run cancelled before child join admission".to_owned();
            let _ = self.supervisor.record_chat_child_failure(
                session,
                handler,
                &child_thread,
                reason.clone(),
            );
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::Interrupted,
                reason,
            );
        }
        let thread_id = child_thread.thread_id.clone();
        let thread_record = BackgroundChatAgentThreadRecord::from_thread(&child_thread);
        if let Err(error) = append_agent_result_continuation(
            session,
            handler,
            thread_id.clone(),
            AgentResultContinuationStatus::Pending,
            Some("registered with the current root-run join barrier".to_owned()),
        ) {
            let _ = self.supervisor.record_chat_child_failure(
                session,
                handler,
                &child_thread,
                format!("failed to persist join dependency: {error:#}"),
            );
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::Internal,
                error.to_string(),
            );
        }
        let (_mailbox_tx, mailbox_rx) = mpsc::channel();
        let child_input = child_input.with_child_cancellation(root_cancellation.clone());
        let run_thread = thread_record.clone();
        let child_session_ref = child_thread.child_session_ref.clone();
        let event_sink = self.background_runs.event_sink();
        let future = Box::pin(async move {
            let cancellation_task_guard = root_cancellation
                .register_task()
                .map_err(|error| anyhow!("root run cancelled before child join start: {error}"))?;
            let _cancellation_task_guard = cancellation_task_guard;
            run_background_chat_agent(
                run_thread,
                child_agent,
                child_session,
                child_session_ref,
                child_input,
                child_options,
                mailbox_rx,
                false,
                event_sink,
            )
            .await
        });
        let sequence = self.next_join_sequence;
        self.next_join_sequence = self.next_join_sequence.saturating_add(1);
        self.join_dependencies.push(JoinedChatAgentHandle {
            sequence,
            call_id: call.id.clone(),
            batch_member,
            thread: thread_record,
            future,
            release_guard: ChatChildThreadGuard {
                supervisor: self.supervisor.clone(),
                thread_id: thread_id.clone(),
            },
        });

        ToolResult::ok(
            call.id.clone(),
            call.name.clone(),
            serde_json::to_string(&json!({
                "thread_id": thread_id.as_str(),
                "status": "running",
                "terminal": false,
                "result_available": false,
                "backgrounded": false,
                "required_before_final": true,
                "host_join_registered": true,
                "next_action": "continue the current tool batch; the host will join this child before the next parent model turn",
                "do_not_call_wait_agent": true,
                "do_not_describe_as_finished": true
            }))
            .unwrap_or_else(|error| format!("failed to serialize agent status: {error}")),
            ToolResultMeta {
                details: json!({
                    "thread_id": thread_id.as_str(),
                    "status": "running",
                    "terminal": false,
                    "result_available": false,
                    "backgrounded": false,
                    "required_before_final": true,
                    "host_join_registered": true,
                }),
                ..ToolResultMeta::default()
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_detachable_chat_child(
        &mut self,
        session: &mut Session,
        call: &ToolCall,
        child_thread: crate::AgentChatChildThread,
        child_agent: Agent<Box<dyn Provider>>,
        child_session: Session,
        child_input: sigil_kernel::AgentRunInput,
        child_options: sigil_kernel::AgentRunOptions,
        _budget_scope_id: TaskId,
        handler: &mut (dyn EventHandler + Send),
    ) -> ToolResult {
        let thread_id = child_thread.thread_id.clone();
        let thread_record = BackgroundChatAgentThreadRecord::from_thread(&child_thread);
        let (_mailbox_tx, mailbox_rx) = mpsc::channel();
        let cancellation_owner = RunCancellationOwner::new();
        let cancellation_handle = cancellation_owner.handle();
        let cancellation_task_guard = cancellation_handle
            .register_task()
            .expect("new background cancellation owner must admit its first task");
        let child_input = child_input.with_cancellation(cancellation_handle);
        let run_thread = thread_record.clone();
        let child_session_ref = child_thread.child_session_ref.clone();
        let event_sink = self.background_runs.event_sink();
        let (start_tx, start_rx) = tokio::sync::oneshot::channel();
        let handle =
            BackgroundChatAgentTask::spawn(thread_id.clone(), event_sink.clone(), async move {
                let _cancellation_task_guard = cancellation_task_guard;
                start_rx
                    .await
                    .map_err(|_| anyhow!("background agent start gate was cancelled"))?;
                run_background_chat_agent(
                    run_thread,
                    child_agent,
                    child_session,
                    child_session_ref,
                    child_input,
                    child_options,
                    mailbox_rx,
                    false,
                    event_sink,
                )
                .await
            });
        if let Err(error) = self.background_runs.insert(
            thread_id.clone(),
            BackgroundChatAgentHandle {
                thread: thread_record,
                handle,
                collection_supervisor: self
                    .supervisor
                    .clone()
                    .with_background_runs(AgentToolBackgroundRuns::default()),
                cancellation_owner,
                write_owner: None,
            },
        ) {
            drop(start_tx);
            let _ = self.supervisor.record_chat_child_failure(
                session,
                handler,
                &child_thread,
                format!("{error:#}"),
            );
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::Internal,
                error.to_string(),
            );
        }
        let _ = start_tx.send(());
        let projection = session.agent_thread_state_projection();
        if let Some(thread) = projection.threads.get(&thread_id) {
            return agent_status_tool_result(session, call, thread);
        }
        ToolResult::ok(
            call.id.clone(),
            call.name.clone(),
            serde_json::to_string(&json!({
                "thread_id": thread_id.as_str(),
                "status": "running",
                "terminal": false,
                "result_available": false,
                "backgrounded": false,
                "required_before_final": true,
                "retry_after_ms": WAIT_AGENT_RUNNING_RETRY_AFTER_MS,
                "next_action": "continue only non-overlapping parent work; use wait_agent before the final answer",
                "do_not_describe_as_finished": true
            }))
            .unwrap_or_else(|error| format!("failed to serialize agent status: {error}")),
            ToolResultMeta {
                details: json!({
                    "thread_id": thread_id.as_str(),
                    "status": "running",
                    "terminal": false,
                    "result_available": false,
                    "backgrounded": false,
                    "required_before_final": true,
                    "retry_after_ms": WAIT_AGENT_RUNNING_RETRY_AFTER_MS,
                }),
                ..ToolResultMeta::default()
            },
        )
    }

    pub(super) async fn run_chat_agent(
        &mut self,
        session: &mut Session,
        call: &ToolCall,
        request: ChatAgentRunRequest,
        options: &sigil_kernel::AgentRunOptions,
        handler: &mut (dyn EventHandler + Send),
        approval_handler: &mut (dyn ApprovalHandler + Send),
    ) -> Result<AgentThreadId> {
        let role = request.resolved_profile.execution_role;
        if matches!(request.mode, AgentInvocationMode::Background) {
            return Err(anyhow!(
                "background agent mode requires provider-backed agent mailbox support"
            ));
        }

        let isolation = default_spawn_isolation(role, &request.resolved_profile);
        let changeset_only_write = isolation == SpawnIsolation::ChangesetOnly;
        let profile_tool_scope = request.resolved_profile.profile.tool_scope.clone();
        let child_registry = child_tool_registry_for_profile(
            &self.base_registry,
            &self.root_config,
            role,
            isolation,
            profile_tool_scope,
        );
        let authority = DelegationAuthority::UserExplicit;
        let effective_multi_agent_mode = self.enforce_effective_multi_agent_mode(session)?;
        admit_model_agent_spawn(
            effective_multi_agent_mode,
            &authority,
            &request.resolved_profile,
            &child_registry,
        )?;
        let thread_id = chat_agent_thread_id_for_call(&call.id, &request.profile_id)?;
        let root_logical_run_id = self
            .root_logical_run_id
            .clone()
            .ok_or_else(|| anyhow!("manual agent invocation is missing the root logical-run id"))?;
        let root_cancellation = self.run_cancellation.clone().ok_or_else(|| {
            anyhow!("manual agent invocation is missing the root cancellation scope")
        })?;
        let source_message_id = session
            .entries()
            .iter()
            .rev()
            .find_map(|entry| match entry {
                SessionLogEntry::User(message) => Some(message.id.clone()),
                _ => None,
            })
            .ok_or_else(|| anyhow!("manual agent invocation is missing its durable source turn"))?;
        let delegation_context = AgentDelegationRunContext {
            source: AgentInvocationGrantSource::Conversation {
                source_turn: sigil_kernel::ConversationTurnRef::new(
                    session.session_scope_id(),
                    source_message_id,
                    root_logical_run_id.clone(),
                )?,
            },
            authority: authority.clone(),
        };
        let isolation = if changeset_only_write {
            TaskIsolationMode::ChangesetOnly
        } else {
            TaskIsolationMode::SharedReadOnly
        };
        let grant = mint_agent_invocation_grant(
            delegation_context.clone(),
            &root_logical_run_id,
            &root_cancellation,
            request.profile_id.clone(),
            role,
            isolation,
            &child_registry,
            options,
            unix_time_ms(),
        )?;
        revalidate_agent_invocation_grant(
            &grant,
            &delegation_context,
            &root_logical_run_id,
            &root_cancellation,
            &request.profile_id,
            role,
            isolation,
            &child_registry,
            &options.workspace_root,
            unix_time_ms(),
        )?;
        let delegation_admission = delegation_admission_entry(
            &grant,
            thread_id.clone(),
            request.profile_id.clone(),
            request.mode,
            request.invocation_source,
            &request.objective,
        )?;
        let child_provider = self
            .provider_factory
            .build_provider(&self.root_config, role, &request.profile_id)
            .await
            .with_context(|| {
                format!(
                    "failed to build child agent provider for {}",
                    request.profile_id.as_str()
                )
            })?;
        let child_capabilities = child_provider.capabilities();
        let parent_session_ref = parent_session_ref(session)?;
        let child_session_ref = agent_child_session_ref(&thread_id)?;
        let budget_scope_id = chat_budget_scope_id(&call.id)?;
        let parent_thread_id = AgentThreadId::new(MAIN_THREAD_ID)?;
        let child_thread = self.supervisor.begin_chat_child_thread(
            session,
            handler,
            crate::AgentChatChildStart {
                call_id: call.id.clone(),
                budget_scope_id: budget_scope_id.clone(),
                parent_thread_id,
                parent_depth: 0,
                batch_id: None,
                batch_member_key: None,
                parent_session_ref,
                profile_id: request.profile_id.clone(),
                role,
                child_session_ref: child_session_ref.clone(),
                objective: request.objective.clone(),
                prompt: request.prompt.clone(),
                workspace_root: options.workspace_root.clone(),
                provider_capabilities: child_capabilities,
                invocation_mode: request.mode,
                invocation_source: request.invocation_source,
                invocation_grant: grant.clone(),
                delegation_admission,
                display_name_hint: request.display_name_hint.clone(),
            },
        )?;
        let mut child_session = match build_agent_child_session(session, &child_session_ref) {
            Ok(session) => session,
            Err(error) => {
                let _ = self.supervisor.record_chat_child_failure(
                    session,
                    handler,
                    &child_thread,
                    format!("{error:#}"),
                );
                return Err(error);
            }
        };
        let child_agent =
            crate::configured_agent(&self.root_config, child_provider, child_registry)
                .context("failed to configure child agent recovery")?;
        let mut child_messages = Vec::new();
        if let Some(system_prompt) = agent_profile_system_prompt(&request.resolved_profile) {
            child_messages.push(ModelMessage::system(system_prompt));
        }
        if changeset_only_write {
            child_messages.push(ModelMessage::system(
                changeset_only_child_contract_prompt().to_owned(),
            ));
        }
        child_messages.push(ModelMessage::user(request.prompt.clone()));
        let child_input = self
            .inherit_root_budgets(sigil_kernel::AgentRunInput::without_persisted_user_message(
                child_messages,
            ))
            .with_agent_invocation_grant(grant.clone());
        let mut child_options = build_role_run_options(
            &self.root_config,
            options.workspace_root.clone(),
            options.interaction_mode,
            role,
        );
        apply_child_permission_constraints(
            &mut child_options,
            options,
            role,
            request.resolved_profile.profile.permission_policy.clone(),
            &grant,
        );
        let child_input = self
            .run_cancellation
            .as_ref()
            .map_or(child_input.clone(), |handle| {
                child_input.with_child_cancellation(handle.clone())
            });
        let _thread_guard = ChatChildThreadGuard {
            supervisor: self.supervisor.clone(),
            thread_id: child_thread.thread_id.clone(),
        };
        let changeset_only_base_snapshot_id = if changeset_only_write {
            Some(capture_chat_changeset_only_parent_snapshot_id(
                session,
                &child_thread.thread_id,
                &options.workspace_root,
                "base",
            )?)
        } else {
            None
        };
        let output = {
            let mut child_handler = ChatChildEventHandler { inner: handler };
            let mut route_handler = ChatAgentApprovalRouteHandler {
                inner: approval_handler,
                parent_session: session,
                source_thread_id: child_thread.thread_id.clone(),
            };
            child_agent
                .run_with_approval_input(
                    &mut child_session,
                    child_input,
                    child_options,
                    &mut child_handler,
                    &mut route_handler,
                )
                .await
        };
        let output = match output {
            Ok(output) => output,
            Err(error) => {
                let _ = self.supervisor.record_chat_child_failure(
                    session,
                    handler,
                    &child_thread,
                    format!("{error:#}"),
                );
                return Err(error).context("child agent failed");
            }
        };
        let materialized = materialize_child_agent_final_answer(
            &mut child_session,
            &child_thread.child_session_ref,
            &child_thread.thread_id,
            &output.result,
        )
        .await?;
        let outcome = output.outcome;
        let changeset_only_controls =
            if let Some(base_snapshot_id) = changeset_only_base_snapshot_id {
                Some(
                    prepare_chat_changeset_only_child_controls(
                        session,
                        &child_thread.thread_id,
                        &base_snapshot_id,
                        &materialized.final_text,
                        &outcome,
                        &options.workspace_root,
                    )
                    .inspect_err(|error| {
                        let _ = self.supervisor.record_chat_child_failure(
                            session,
                            handler,
                            &child_thread,
                            format!("{error:#}"),
                        );
                    })?,
                )
            } else {
                None
            };
        let usage = usage_summary_from_stats(child_session.stats());
        let budget_warning = self
            .supervisor
            .validate_usage_budget(&budget_scope_id, &usage)
            .err()
            .map(|error| format!("{error:#}"));
        let status = child_status_from_outcome(&materialized.final_text, &outcome);
        self.supervisor.record_chat_child_result(
            session,
            handler,
            &child_thread,
            status,
            &materialized,
            &outcome,
            Some(usage),
        )?;
        if let Some(controls) = changeset_only_controls {
            append_chat_changeset_only_child_controls(session, handler, controls)?;
        }
        if let Some(warning) = budget_warning {
            let _ = handler.handle(RunEvent::Notice(format!(
                "agent budget warning after child completion: {warning}"
            )));
        }
        Ok(child_thread.thread_id)
    }
}

pub(super) fn profile_uses_changeset_only_write(
    role: AgentRole,
    profile: &ResolvedAgentProfile,
) -> bool {
    role == AgentRole::SubagentWrite
        && profile.profile.result_policy == sigil_kernel::AgentResultPolicy::ForegroundMergeRequired
}

pub(super) fn child_tool_registry_for_profile(
    base_registry: &ToolRegistry,
    root_config: &RootConfig,
    role: AgentRole,
    isolation: SpawnIsolation,
    profile_tool_scope: sigil_kernel::ToolRegistryScope,
) -> ToolRegistry {
    let base_registry = base_registry.snapshot();
    let registry = match isolation {
        SpawnIsolation::ChangesetOnly => changeset_only_child_tool_registry(&base_registry),
        SpawnIsolation::SharedReadOnly => {
            build_role_tool_registry(&base_registry, root_config, role)
                .into_registry()
                .scoped(crate::run_options::read_only_role_tool_scope())
                .into_registry()
        }
        SpawnIsolation::Worktree => {
            build_role_tool_registry(&base_registry, root_config, role).into_registry()
        }
    };
    registry.scoped(profile_tool_scope).into_registry()
}

fn default_spawn_isolation(role: AgentRole, profile: &ResolvedAgentProfile) -> SpawnIsolation {
    if profile_uses_changeset_only_write(role, profile) {
        SpawnIsolation::ChangesetOnly
    } else {
        SpawnIsolation::SharedReadOnly
    }
}

async fn prepare_chat_worktree(
    session: &mut Session,
    handler: &mut (dyn EventHandler + Send),
    thread_id: &AgentThreadId,
    workspace_root: &Path,
) -> Result<crate::isolated_workspace::MaterializedGitWorktree> {
    let base_snapshot_id =
        capture_chat_changeset_only_parent_snapshot_id(session, thread_id, workspace_root, "base")?;
    let recorder = session
        .mutation_event_recorder()
        .ok_or_else(|| anyhow!("worktree chat child requires a durable parent session store"))?;
    let operation_id = format!(
        "chat-worktree-overlay-{}",
        stable_event_uuid(
            "sigil-chat-worktree-overlay",
            &format!("{}:{}", thread_id.as_str(), base_snapshot_id),
        )
    );
    let lease_recorder = recorder.clone();
    let lease_workspace_root = workspace_root.to_path_buf();
    let lease_operation_id = operation_id.clone();
    let _lease = tokio::task::spawn_blocking(move || {
        lease_recorder.coordinator_with_workspace_lease(
            lease_workspace_root,
            lease_operation_id,
            None,
        )
    })
    .await
    .context("chat worktree mutation lease task failed")??;
    let frozen = crate::isolated_workspace::freeze_git_worktree_base(
        crate::isolated_workspace::GitWorktreeBaseFreezeRequest {
            parent_workspace_root: workspace_root.to_path_buf(),
            base_snapshot_id: base_snapshot_id.clone(),
            operation_id,
            artifact_recorder: recorder,
        },
    )
    .await?;
    let isolated_workspace_id = chat_worktree_id(thread_id);
    let parent_workspace_id = stable_workspace_id(workspace_root)?;
    let owner_agent_id = format!("agent:{}", thread_id.as_str());
    append_control_to_parent(
        session,
        handler,
        ControlEntry::IsolatedWorkspacePrepared(IsolatedWorkspacePrepared {
            isolated_workspace_id: isolated_workspace_id.clone(),
            parent_workspace_id: parent_workspace_id.clone(),
            owner_agent_id: owner_agent_id.clone(),
            isolation_mode: WriteIsolationMode::Worktree,
            base_snapshot_id: base_snapshot_id.clone(),
            backend: IsolatedWorkspaceBackend::GitWorktree,
            base_commit: Some(frozen.base_commit().to_owned()),
            overlay_digest: Some(frozen.overlay_digest().to_owned()),
            overlay_artifact_ref: Some(frozen.overlay_artifact_ref().clone()),
            overlay_content_artifact_refs: frozen.overlay_content_artifact_refs(),
            overlay_entry_count: frozen.overlay_entry_count(),
        }),
    )?;
    let materialized = match crate::isolated_workspace::materialize_git_worktree_from_frozen_base(
        &frozen,
        isolated_workspace_id.clone(),
    )
    .await
    {
        Ok(materialized) => materialized,
        Err(error) => {
            append_control_to_parent(
                session,
                handler,
                ControlEntry::IsolatedWorkspaceCleanupRecorded(IsolatedWorkspaceCleanupRecorded {
                    isolated_workspace_id,
                    status: IsolatedWorkspaceCleanupStatus::Failed,
                }),
            )?;
            return Err(error);
        }
    };
    let created = IsolatedWorkspaceCreated {
        isolated_workspace_id: materialized.isolated_workspace_id().to_owned(),
        parent_workspace_id,
        owner_agent_id,
        isolation_mode: WriteIsolationMode::Worktree,
        base_snapshot_id,
        backend: IsolatedWorkspaceBackend::GitWorktree,
        base_commit: Some(materialized.base_commit().to_owned()),
        baseline_tree: Some(materialized.baseline_tree().to_owned()),
        overlay_digest: materialized.overlay_digest().map(str::to_owned),
        overlay_artifact_ref: materialized.overlay_artifact_ref().cloned(),
        overlay_content_artifact_refs: materialized.overlay_content_artifact_refs().to_vec(),
        overlay_entry_count: materialized.overlay_entry_count(),
        materialized_snapshot_id: Some(materialized.child_snapshot_id().to_owned()),
    };
    if let Err(error) = append_control_to_parent(
        session,
        handler,
        ControlEntry::IsolatedWorkspaceCreated(created),
    ) {
        let isolated_workspace_id = materialized.isolated_workspace_id().to_owned();
        let cleanup_error = materialized.cleanup().await.err();
        let _ = append_control_to_parent(
            session,
            handler,
            ControlEntry::IsolatedWorkspaceCleanupRecorded(IsolatedWorkspaceCleanupRecorded {
                isolated_workspace_id,
                status: if cleanup_error.is_some() {
                    IsolatedWorkspaceCleanupStatus::Failed
                } else {
                    IsolatedWorkspaceCleanupStatus::Removed
                },
            }),
        );
        return Err(match cleanup_error {
            Some(cleanup_error) => error.context(cleanup_error),
            None => error,
        });
    }
    Ok(materialized)
}

fn chat_worktree_id(thread_id: &AgentThreadId) -> String {
    format!(
        "worktree-{}",
        stable_event_uuid("sigil-chat-worktree", thread_id.as_str())
    )
}

pub(super) async fn cleanup_chat_worktree(
    session: &mut Session,
    handler: &mut (dyn EventHandler + Send),
    materialized: crate::isolated_workspace::MaterializedGitWorktree,
) -> Result<()> {
    let isolated_workspace_id = materialized.isolated_workspace_id().to_owned();
    let (status, error) = match materialized.cleanup().await {
        Ok(receipt) => (receipt.status, None),
        Err(error) => (IsolatedWorkspaceCleanupStatus::Failed, Some(error)),
    };
    append_control_to_parent(
        session,
        handler,
        ControlEntry::IsolatedWorkspaceCleanupRecorded(IsolatedWorkspaceCleanupRecorded {
            isolated_workspace_id,
            status,
        }),
    )?;
    if let Some(error) = error {
        Err(error.context("chat child worktree cleanup failed"))
    } else {
        Ok(())
    }
}

pub(super) struct PreparedChatWorktreeControls {
    change_set: ChangeSet,
    isolated: IsolatedChangeSetProduced,
    merge_review: MergeReviewRequested,
}

pub(super) async fn prepare_chat_worktree_child_controls(
    session: &Session,
    thread_id: &AgentThreadId,
    worktree: &crate::isolated_workspace::MaterializedGitWorktree,
    _outcome: &sigil_kernel::AgentRunOutcome,
    workspace_root: &Path,
    objective: &str,
) -> Result<Option<PreparedChatWorktreeControls>> {
    let change_set_id = ChangeSetId::new(format!(
        "changeset-{}",
        stable_event_uuid("sigil-chat-worktree-changeset", thread_id.as_str())
    ))?;
    let Some(mut proposal) = worktree
        .extract_changeset(
            change_set_id,
            objective.to_owned(),
            "Model-selected worktree child proposal",
        )
        .await?
    else {
        return Ok(None);
    };
    let observed_digest = format!("{:x}", Sha256::digest(proposal.artifact.content.as_bytes()));
    if observed_digest != proposal.artifact.content_sha256 {
        bail!("worktree child changeset artifact digest changed before persistence");
    }
    let recorder = session
        .mutation_event_recorder()
        .ok_or_else(|| anyhow!("worktree child changeset requires durable storage"))?;
    let workspace_id = stable_workspace_id(workspace_root)?;
    let operation_id = format!(
        "chat-worktree-changeset-artifact-{}",
        stable_event_uuid(
            "sigil-chat-worktree-changeset-artifact",
            &format!("{}:{}", thread_id.as_str(), proposal.change_set.id.as_str()),
        )
    );
    let bytes = proposal.artifact.content.as_bytes().to_vec();
    let artifact_ref = tokio::task::spawn_blocking(move || {
        recorder.capture_immutable_content_artifact(
            &workspace_id,
            &operation_id,
            Path::new(".sigil-agent-artifacts/chat-worktree.diff"),
            &bytes,
        )
    })
    .await
    .context("chat worktree changeset artifact persistence task failed")??;
    proposal.artifact_ref = artifact_ref;
    proposal.integration_facts.changeset_artifact_ref = proposal.artifact_ref.clone();
    let touched_subjects = changeset_touched_subjects(&proposal.change_set);
    let changeset_id = proposal.change_set.id.clone();
    let after_snapshot_id = capture_chat_changeset_only_parent_snapshot_id(
        session,
        thread_id,
        workspace_root,
        "after",
    )?;
    let merge_review_id = chat_changeset_only_merge_review_id(thread_id, &proposal.change_set)?;
    Ok(Some(PreparedChatWorktreeControls {
        change_set: proposal.change_set,
        isolated: IsolatedChangeSetProduced {
            changeset_id: changeset_id.clone(),
            owner_agent_id: format!("agent:{}", thread_id.as_str()),
            base_snapshot_id: worktree.base_snapshot_id().to_owned(),
            child_snapshot_id: proposal.child_snapshot_id,
            source_isolation: WriteIsolationMode::Worktree,
            artifact_ref: Some(proposal.artifact_ref),
            touched_subjects,
            integration_facts: proposal.integration_facts,
        },
        merge_review: MergeReviewRequested {
            review_id: merge_review_id,
            changeset_id,
            parent_workspace_snapshot_id: after_snapshot_id,
        },
    }))
}

pub(super) fn append_chat_isolated_child_controls(
    session: &mut Session,
    handler: &mut (dyn EventHandler + Send),
    controls: PreparedChatWorktreeControls,
) -> Result<()> {
    append_chat_changeset_controls(
        session,
        handler,
        controls.change_set,
        controls.isolated,
        controls.merge_review,
    )
}

fn append_chat_changeset_controls(
    session: &mut Session,
    handler: &mut (dyn EventHandler + Send),
    change_set: ChangeSet,
    isolated: IsolatedChangeSetProduced,
    merge_review: MergeReviewRequested,
) -> Result<()> {
    handler.commit_controls(
        session,
        vec![
            ControlEntry::ChangeSetProposed(change_set),
            ControlEntry::IsolatedChangeSetProduced(isolated),
            ControlEntry::MergeReviewRequested(merge_review),
        ],
    )?;
    Ok(())
}

pub(super) enum PreparedChatIsolatedChildControls {
    ChangesetOnly(PreparedChatChangesetOnlyControls),
    Worktree(PreparedChatWorktreeControls),
}

pub(super) fn append_prepared_chat_isolated_child_controls(
    session: &mut Session,
    handler: &mut (dyn EventHandler + Send),
    controls: PreparedChatIsolatedChildControls,
) -> Result<()> {
    match controls {
        PreparedChatIsolatedChildControls::ChangesetOnly(controls) => {
            append_chat_changeset_only_child_controls(session, handler, controls)
        }
        PreparedChatIsolatedChildControls::Worktree(controls) => {
            append_chat_isolated_child_controls(session, handler, controls)
        }
    }
}

fn worktree_preparation_unavailable_tool_result(
    call: &ToolCall,
    error: &anyhow::Error,
) -> ToolResult {
    let message = format!("model-selected worktree isolation could not be prepared: {error:#}");
    ToolResult::error(
        call.id.clone(),
        call.name.clone(),
        ToolErrorKind::Unsupported,
        serde_json::to_string(&json!({
            "error": "worktree_isolation_unavailable",
            "message": message,
            "supported_modes": ["foreground", "join_before_final", "background"],
            "next_action": "retry in a Git repository with a durable parent session or choose changeset_only"
        }))
        .unwrap_or_else(|serialize_error| format!("failed to serialize worktree rejection: {serialize_error}")),
    )
    .with_error_details(
        true,
        json!({
            "error": "worktree_isolation_unavailable",
            "supported_modes": ["foreground", "join_before_final", "background"],
        }),
    )
}

fn capture_chat_changeset_only_parent_snapshot_id(
    session: &Session,
    thread_id: &AgentThreadId,
    workspace_root: &Path,
    label: &str,
) -> Result<String> {
    let scope = VerificationScope::all_tracked(DEFAULT_TASK_VERIFICATION_SCOPE_HASH);
    let workspace_id = stable_workspace_id(workspace_root)?;
    let seed = format!("{}:{}:{}", thread_id.as_str(), workspace_id, label);
    let source_event_id = format!(
        "chat-changeset-only-{label}-snapshot-{}",
        stable_event_uuid("sigil-chat-changeset-only-snapshot", &seed)
    );
    let snapshot = build_workspace_snapshot_for_event(
        workspace_root,
        workspace_id,
        &scope,
        0,
        source_event_id,
        session.next_stream_sequence_hint().unwrap_or(1),
    )?;
    snapshot.workspace_snapshot_id.ok_or_else(|| {
        anyhow!(
            "changeset-only chat worker {} cannot bind {label} parent workspace snapshot",
            thread_id.as_str()
        )
    })
}

pub(super) struct PreparedChatChangesetOnlyControls {
    change_set: ChangeSet,
    isolated: IsolatedChangeSetProduced,
    merge_review: MergeReviewRequested,
}

pub(super) fn prepare_chat_changeset_only_child_controls(
    session: &Session,
    thread_id: &AgentThreadId,
    base_snapshot_id: &str,
    final_text: &str,
    outcome: &sigil_kernel::AgentRunOutcome,
    workspace_root: &Path,
) -> Result<PreparedChatChangesetOnlyControls> {
    if !outcome.changed_files.is_empty() {
        bail!(
            "changeset-only chat worker {} mutated parent workspace files: {}",
            thread_id.as_str(),
            outcome.changed_files.join(", ")
        );
    }
    let after_snapshot_id = capture_chat_changeset_only_parent_snapshot_id(
        session,
        thread_id,
        workspace_root,
        "after",
    )?;
    if after_snapshot_id != base_snapshot_id {
        bail!(
            "changeset-only chat worker {} changed parent workspace snapshot",
            thread_id.as_str()
        );
    }
    let proposal = decode_changeset_only_child_output(final_text)?;
    let touched_subjects = changeset_touched_subjects(&proposal.change_set);
    let changeset_id = proposal.change_set.id.clone();
    let merge_review_id = chat_changeset_only_merge_review_id(thread_id, &proposal.change_set)?;
    Ok(PreparedChatChangesetOnlyControls {
        change_set: proposal.change_set,
        isolated: IsolatedChangeSetProduced {
            changeset_id: changeset_id.clone(),
            owner_agent_id: format!("agent:{}", thread_id.as_str()),
            base_snapshot_id: base_snapshot_id.to_owned(),
            child_snapshot_id: None,
            source_isolation: WriteIsolationMode::ChangesetOnly,
            artifact_ref: Some(proposal.artifact_ref),
            touched_subjects,
            integration_facts: proposal.integration_facts,
        },
        merge_review: MergeReviewRequested {
            review_id: merge_review_id,
            changeset_id,
            parent_workspace_snapshot_id: after_snapshot_id,
        },
    })
}

pub(super) fn append_chat_changeset_only_child_controls(
    session: &mut Session,
    handler: &mut (dyn EventHandler + Send),
    controls: PreparedChatChangesetOnlyControls,
) -> Result<()> {
    append_chat_changeset_controls(
        session,
        handler,
        controls.change_set,
        controls.isolated,
        controls.merge_review,
    )
}

fn append_control_to_parent(
    session: &mut Session,
    handler: &mut (dyn EventHandler + Send),
    control: ControlEntry,
) -> Result<()> {
    handler.commit_controls(session, vec![control])?;
    Ok(())
}

fn chat_changeset_only_merge_review_id(
    thread_id: &AgentThreadId,
    change_set: &ChangeSet,
) -> Result<MergeReviewId> {
    MergeReviewId::new(format!(
        "review-{}",
        stable_event_uuid(
            "sigil-chat-merge-review",
            &format!("{}:{}", thread_id.as_str(), change_set.id.as_str())
        )
    ))
}

fn changeset_touched_subjects(change_set: &ChangeSet) -> Vec<MutationSubject> {
    change_set
        .files
        .iter()
        .flat_map(|file| {
            let mut subjects = vec![MutationSubject::File {
                path: PathBuf::from(file.path.trim()),
                file_type: FileType::File,
            }];
            if let Some(previous_path) = &file.previous_path {
                subjects.push(MutationSubject::File {
                    path: PathBuf::from(previous_path.trim()),
                    file_type: FileType::File,
                });
            }
            subjects
        })
        .collect()
}

pub(super) fn spawn_scope_overlap_warning(
    session: &Session,
    parsed: &SpawnAgentArgs,
) -> Option<String> {
    let parent_prompt = session.entries().iter().rev().find_map(|entry| {
        let SessionLogEntry::User(message) = entry else {
            return None;
        };
        message.content.as_deref()
    })?;
    let parent_tokens = scope_tokens(parent_prompt);
    if parent_tokens.is_empty() {
        return None;
    }
    let child_tokens = scope_tokens(&format!("{}\n{}", parsed.objective, parsed.prompt));
    let overlap = parent_tokens
        .intersection(&child_tokens)
        .take(4)
        .cloned()
        .collect::<Vec<_>>();
    if overlap.is_empty() {
        return None;
    }
    Some(format!(
        "agent scope overlap warning: child objective references parent scope tokens {}; keep parent work non-overlapping or wait/read the child result before final.",
        overlap.join(", ")
    ))
}

fn scope_tokens(text: &str) -> std::collections::BTreeSet<String> {
    text.split(|character: char| {
        character.is_whitespace()
            || matches!(
                character,
                '"' | '\'' | '`' | ',' | ';' | '(' | ')' | '[' | ']'
            )
    })
    .filter_map(|raw| {
        let token = raw.trim_matches(|character: char| {
            matches!(
                character,
                ':' | '.' | ',' | ';' | ')' | '(' | '[' | ']' | '<' | '>' | '。' | '，'
            )
        });
        let token = token.trim_start_matches("./");
        let looks_like_scope = token.contains('/')
            || token.ends_with(".rs")
            || token.ends_with(".md")
            || token.ends_with(".toml")
            || token.ends_with(".sh")
            || token.ends_with(".json")
            || token.ends_with(".yaml")
            || token.ends_with(".yml");
        looks_like_scope.then(|| token.to_owned())
    })
    .collect()
}

#[cfg(test)]
#[path = "tests/spawn_tests.rs"]
mod tests;
