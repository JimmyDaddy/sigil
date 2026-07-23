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
        let changeset_only_write = profile_uses_changeset_only_write(role, &resolved_profile);
        if changeset_only_write && matches!(parsed.mode, AgentInvocationMode::Background) {
            return unsupported_background_write_tool_result(call, &parsed.profile_id);
        }
        let profile_tool_scope = resolved_profile.profile.tool_scope.clone();
        let child_registry = child_tool_registry_for_profile(
            &self.base_registry,
            &self.root_config,
            role,
            changeset_only_write,
            profile_tool_scope,
        );
        let authority = self.model_delegation_authority();
        if let Err(error) = admit_model_agent_spawn(
            self.root_config.task.multi_agent_mode,
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
        let delegation_admission = match delegation_admission_entry(
            authority,
            thread_id.clone(),
            parsed.profile_id.clone(),
            parsed.mode,
            AgentInvocationSource::Chat,
            &parsed.objective,
            &child_registry,
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
        let child_provider =
            match self
                .provider_factory
                .build_provider(&self.root_config, role, &parsed.profile_id)
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
                parent_session_ref,
                profile_id: parsed.profile_id.clone(),
                role,
                child_session_ref: child_session_ref.clone(),
                objective: parsed.objective.clone(),
                prompt: parsed.prompt.clone(),
                workspace_root: options.workspace_root.clone(),
                provider_capabilities: child_capabilities,
                invocation_mode: parsed.mode,
                invocation_source: AgentInvocationSource::Chat,
                delegation_admission,
                display_name_hint: parsed.display_name_hint.clone(),
            },
        ) {
            Ok(thread) => thread,
            Err(error) => {
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
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    error.to_string(),
                );
            }
        };
        let child_agent = Agent::new(child_provider, child_registry);
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
        let child_input = self.inherit_web_task_tree_budget(
            sigil_kernel::AgentRunInput::without_persisted_user_message(child_messages),
        );
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
            resolved_profile.profile.permission_policy.clone(),
        );

        if matches!(parsed.mode, AgentInvocationMode::Background) {
            let Some(mailbox_rx) = child_thread.mailbox_rx.take() else {
                let _ = self.supervisor.record_chat_child_failure(
                    session,
                    handler,
                    &child_thread,
                    "background agent mailbox was not created".to_owned(),
                );
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    "background agent mailbox was not created",
                );
            };
            let thread_id = child_thread.thread_id.clone();
            let cancellation_owner = RunCancellationOwner::new();
            let cancellation_handle = cancellation_owner.handle();
            let cancellation_task_guard = cancellation_handle
                .register_task()
                .expect("new background cancellation owner must admit its first task");
            let child_input = child_input.with_cancellation(cancellation_handle);
            let run_thread_id = thread_id.clone();
            let thread_record = BackgroundChatAgentThreadRecord::from_thread(&child_thread);
            let child_session_ref = child_thread.child_session_ref.clone();
            let event_sink = self.background_runs.event_sink();
            let handle = tokio::spawn(async move {
                let _cancellation_task_guard = cancellation_task_guard;
                run_background_chat_agent(
                    run_thread_id,
                    child_agent,
                    child_session,
                    child_session_ref,
                    child_input,
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
            let projection = session.agent_thread_state_projection();
            if let Some(thread) = projection.threads.get(&thread_id) {
                return agent_status_tool_result(call, thread);
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
            && !changeset_only_write
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
            );
        }

        if self.run_cancellation.is_none()
            && !changeset_only_write
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

        let changeset_only_base_snapshot_id = match changeset_only_write {
            true => match capture_chat_changeset_only_parent_snapshot_id(
                session,
                &child_thread.thread_id,
                options,
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
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    error.to_string(),
                );
            }
        };
        let outcome = output.outcome;
        let changeset_only_controls =
            if let Some(base_snapshot_id) = changeset_only_base_snapshot_id {
                match prepare_chat_changeset_only_child_controls(
                    session,
                    &child_thread.thread_id,
                    &base_snapshot_id,
                    &materialized.final_text,
                    &outcome,
                    options,
                ) {
                    Ok(controls) => Some(controls),
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
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::Internal,
                error.to_string(),
            );
        }
        if let Some(controls) = changeset_only_controls
            && let Err(error) =
                append_chat_changeset_only_child_controls(session, handler, controls)
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
    fn start_joined_chat_child(
        &mut self,
        session: &mut Session,
        call: &ToolCall,
        child_thread: crate::AgentChatChildThread,
        child_agent: Agent<Box<dyn Provider>>,
        child_session: Session,
        child_input: sigil_kernel::AgentRunInput,
        child_options: sigil_kernel::AgentRunOptions,
        handler: &mut (dyn EventHandler + Send),
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
        let run_thread_id = thread_id.clone();
        let child_session_ref = child_thread.child_session_ref.clone();
        let event_sink = self.background_runs.event_sink();
        let future = Box::pin(async move {
            let cancellation_task_guard = root_cancellation
                .register_task()
                .map_err(|error| anyhow!("root run cancelled before child join start: {error}"))?;
            let _cancellation_task_guard = cancellation_task_guard;
            run_background_chat_agent(
                run_thread_id,
                child_agent,
                child_session,
                child_session_ref,
                child_input,
                child_options,
                mailbox_rx,
                event_sink,
            )
            .await
        });
        let sequence = self.next_join_sequence;
        self.next_join_sequence = self.next_join_sequence.saturating_add(1);
        self.join_dependencies.push(JoinedChatAgentHandle {
            sequence,
            call_id: call.id.clone(),
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
        let run_thread_id = thread_id.clone();
        let child_session_ref = child_thread.child_session_ref.clone();
        let event_sink = self.background_runs.event_sink();
        let handle = tokio::spawn(async move {
            let _cancellation_task_guard = cancellation_task_guard;
            run_background_chat_agent(
                run_thread_id,
                child_agent,
                child_session,
                child_session_ref,
                child_input,
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
        let projection = session.agent_thread_state_projection();
        if let Some(thread) = projection.threads.get(&thread_id) {
            return agent_status_tool_result(call, thread);
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

        let changeset_only_write =
            profile_uses_changeset_only_write(role, &request.resolved_profile);
        let profile_tool_scope = request.resolved_profile.profile.tool_scope.clone();
        let child_registry = child_tool_registry_for_profile(
            &self.base_registry,
            &self.root_config,
            role,
            changeset_only_write,
            profile_tool_scope,
        );
        let authority = DelegationAuthority::UserExplicit;
        admit_model_agent_spawn(
            self.root_config.task.multi_agent_mode,
            &authority,
            &request.resolved_profile,
            &child_registry,
        )?;
        let thread_id = chat_agent_thread_id_for_call(&call.id, &request.profile_id)?;
        let delegation_admission = delegation_admission_entry(
            authority,
            thread_id.clone(),
            request.profile_id.clone(),
            request.mode,
            request.invocation_source,
            &request.objective,
            &child_registry,
        )?;
        let child_provider = self
            .provider_factory
            .build_provider(&self.root_config, role, &request.profile_id)
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
        let child_agent = Agent::new(child_provider, child_registry);
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
        let child_input = self.inherit_web_task_tree_budget(
            sigil_kernel::AgentRunInput::without_persisted_user_message(child_messages),
        );
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
                options,
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
                        options,
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

fn profile_uses_changeset_only_write(role: AgentRole, profile: &ResolvedAgentProfile) -> bool {
    role == AgentRole::SubagentWrite
        && profile.profile.result_policy == sigil_kernel::AgentResultPolicy::ForegroundMergeRequired
}

fn child_tool_registry_for_profile(
    base_registry: &ToolRegistry,
    root_config: &RootConfig,
    role: AgentRole,
    changeset_only_write: bool,
    profile_tool_scope: sigil_kernel::ToolRegistryScope,
) -> ToolRegistry {
    let base_registry = base_registry.snapshot();
    let registry = if changeset_only_write {
        changeset_only_child_tool_registry(&base_registry)
    } else {
        build_role_tool_registry(&base_registry, root_config, role).into_registry()
    };
    registry.scoped(profile_tool_scope).into_registry()
}

fn unsupported_background_write_tool_result(
    call: &ToolCall,
    profile_id: &AgentProfileId,
) -> ToolResult {
    ToolResult::error(
        call.id.clone(),
        call.name.clone(),
        ToolErrorKind::Unsupported,
        serde_json::to_string(&json!({
            "error": "unsupported_write_background_without_isolation",
            "message": "write-capable worker agents require foreground changeset-only isolation until background isolation and merge are available",
            "profile_id": profile_id.as_str(),
            "supported_modes": ["foreground", "join_before_final"]
        }))
        .unwrap_or_else(|error| format!("failed to serialize background write rejection: {error}")),
    )
    .with_error_details(
        true,
        json!({
            "error": "unsupported_write_background_without_isolation",
            "profile_id": profile_id.as_str(),
            "supported_modes": ["foreground", "join_before_final"],
        }),
    )
}

fn capture_chat_changeset_only_parent_snapshot_id(
    session: &Session,
    thread_id: &AgentThreadId,
    options: &sigil_kernel::AgentRunOptions,
    label: &str,
) -> Result<String> {
    let scope = VerificationScope::all_tracked(DEFAULT_TASK_VERIFICATION_SCOPE_HASH);
    let workspace_id = stable_workspace_id(&options.workspace_root)?;
    let seed = format!("{}:{}:{}", thread_id.as_str(), workspace_id, label);
    let source_event_id = format!(
        "chat-changeset-only-{label}-snapshot-{}",
        stable_event_uuid("sigil-chat-changeset-only-snapshot", &seed)
    );
    let snapshot = build_workspace_snapshot_for_event(
        &options.workspace_root,
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

struct PreparedChatChangesetOnlyControls {
    change_set: ChangeSet,
    isolated: IsolatedChangeSetProduced,
    merge_review: MergeReviewRequested,
}

fn prepare_chat_changeset_only_child_controls(
    session: &Session,
    thread_id: &AgentThreadId,
    base_snapshot_id: &str,
    final_text: &str,
    outcome: &sigil_kernel::AgentRunOutcome,
    options: &sigil_kernel::AgentRunOptions,
) -> Result<PreparedChatChangesetOnlyControls> {
    if !outcome.changed_files.is_empty() {
        bail!(
            "changeset-only chat worker {} mutated parent workspace files: {}",
            thread_id.as_str(),
            outcome.changed_files.join(", ")
        );
    }
    let after_snapshot_id =
        capture_chat_changeset_only_parent_snapshot_id(session, thread_id, options, "after")?;
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
        },
        merge_review: MergeReviewRequested {
            review_id: merge_review_id,
            changeset_id,
            parent_workspace_snapshot_id: after_snapshot_id,
        },
    })
}

fn append_chat_changeset_only_child_controls(
    session: &mut Session,
    handler: &mut (dyn EventHandler + Send),
    controls: PreparedChatChangesetOnlyControls,
) -> Result<()> {
    for control in [
        ControlEntry::ChangeSetProposed(controls.change_set),
        ControlEntry::IsolatedChangeSetProduced(controls.isolated),
        ControlEntry::MergeReviewRequested(controls.merge_review),
    ] {
        append_control_to_parent(session, handler, control)?;
    }
    Ok(())
}

fn append_control_to_parent(
    session: &mut Session,
    handler: &mut (dyn EventHandler + Send),
    control: ControlEntry,
) -> Result<()> {
    session.append_control(control.clone())?;
    handler.handle(RunEvent::Control(control))
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

fn spawn_scope_overlap_warning(session: &Session, parsed: &SpawnAgentArgs) -> Option<String> {
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
