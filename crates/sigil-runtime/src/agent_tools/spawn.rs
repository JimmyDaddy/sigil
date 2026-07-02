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
        if let Some(warning) = spawn_scope_overlap_warning(session, &parsed) {
            let _ = handler.handle(RunEvent::Notice(warning));
        }
        let role = role_for_profile_id(&parsed.profile_id);
        let resolved_profile = match self.resolve_spawn_profile(&parsed.profile_id) {
            Ok(profile) => profile,
            Err(error) => {
                return agent_spawn_denied_tool_result(call, format!("{error:#}"));
            }
        };
        let profile_tool_scope = resolved_profile.profile.tool_scope.clone();
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
        let child_registry = build_role_tool_registry(&self.base_registry, &self.root_config, role)
            .into_registry()
            .scoped(profile_tool_scope)
            .into_registry();
        let child_agent = Agent::new(child_provider, child_registry);
        let mut child_messages = Vec::new();
        if let Some(system_prompt) = agent_profile_system_prompt(&resolved_profile) {
            child_messages.push(ModelMessage::system(system_prompt));
        }
        child_messages.push(ModelMessage::user(parsed.prompt.clone()));
        let child_input =
            sigil_kernel::AgentRunInput::without_persisted_user_message(child_messages);
        let mut child_options = build_role_run_options(
            &self.root_config,
            options.workspace_root.clone(),
            options.interaction_mode,
            role,
        );
        child_options.permission_config = effective_child_permission_config(
            &options.permission_config,
            &child_options.permission_config,
            &resolved_profile.profile.permission_policy,
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
            let handle = tokio::spawn(run_background_chat_agent(
                thread_id.clone(),
                child_agent,
                child_session,
                child_thread.child_session_ref.clone(),
                child_input,
                child_options,
                mailbox_rx,
                self.background_runs.event_sink(),
            ));
            if let Err(error) = self.background_runs.insert(
                thread_id.clone(),
                BackgroundChatAgentHandle {
                    thread: BackgroundChatAgentThreadRecord::from_thread(&child_thread),
                    handle,
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
                        "retry_after_ms": 5_000_u64,
                    }),
                    ..ToolResultMeta::default()
                },
            );
        }

        if matches!(parsed.mode, AgentInvocationMode::JoinBeforeFinal)
            && tool_scope_is_safe_readonly_for_auto_spawn(&resolved_profile.profile.tool_scope)
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
        let final_answer_ref =
            agent_final_answer_ref(&child_thread.child_session_ref, &output.result);
        let final_text = output.result.final_text;
        let outcome = output.outcome;
        let usage = usage_summary_from_stats(child_session.stats());
        let budget_warning = self
            .supervisor
            .validate_usage_budget(&budget_scope_id, &usage)
            .err()
            .map(|error| format!("{error:#}"));
        let status = child_status_from_outcome(&final_text, &outcome);
        if let Err(error) = self.supervisor.record_chat_child_result(
            session,
            handler,
            &child_thread,
            status,
            &final_text,
            &outcome,
            Some(usage),
            final_answer_ref,
        ) {
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
        let handle = tokio::spawn(run_background_chat_agent(
            thread_id.clone(),
            child_agent,
            child_session,
            child_thread.child_session_ref.clone(),
            child_input,
            child_options,
            mailbox_rx,
            self.background_runs.event_sink(),
        ));
        if let Err(error) = self.background_runs.insert(
            thread_id.clone(),
            BackgroundChatAgentHandle {
                thread: thread_record,
                handle,
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
                "retry_after_ms": 5_000_u64,
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
                    "retry_after_ms": 5_000_u64,
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
        let role = role_for_profile_id(&request.profile_id);
        if matches!(request.mode, AgentInvocationMode::Background) {
            return Err(anyhow!(
                "background agent mode requires provider-backed agent mailbox support"
            ));
        }

        let profile_tool_scope = request.resolved_profile.profile.tool_scope.clone();
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
        let thread_id = chat_agent_thread_id_for_call(&call.id, &request.profile_id)?;
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
        let child_registry = build_role_tool_registry(&self.base_registry, &self.root_config, role)
            .into_registry()
            .scoped(profile_tool_scope)
            .into_registry();
        let child_agent = Agent::new(child_provider, child_registry);
        let mut child_messages = Vec::new();
        if let Some(system_prompt) = agent_profile_system_prompt(&request.resolved_profile) {
            child_messages.push(ModelMessage::system(system_prompt));
        }
        child_messages.push(ModelMessage::user(request.prompt.clone()));
        let child_input =
            sigil_kernel::AgentRunInput::without_persisted_user_message(child_messages);
        let mut child_options = build_role_run_options(
            &self.root_config,
            options.workspace_root.clone(),
            options.interaction_mode,
            role,
        );
        child_options.permission_config = effective_child_permission_config(
            &options.permission_config,
            &child_options.permission_config,
            &request.resolved_profile.profile.permission_policy,
        );
        if matches!(request.mode, AgentInvocationMode::JoinBeforeFinal)
            && tool_scope_is_safe_readonly_for_auto_spawn(
                &request.resolved_profile.profile.tool_scope,
            )
        {
            let result = self
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
            if result.is_error() {
                return Err(anyhow!(result.content));
            }
            return Ok(thread_id);
        }
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
                return Err(error).context("child agent failed");
            }
        };
        let final_answer_ref =
            agent_final_answer_ref(&child_thread.child_session_ref, &output.result);
        let final_text = output.result.final_text;
        let outcome = output.outcome;
        let usage = usage_summary_from_stats(child_session.stats());
        let budget_warning = self
            .supervisor
            .validate_usage_budget(&budget_scope_id, &usage)
            .err()
            .map(|error| format!("{error:#}"));
        let status = child_status_from_outcome(&final_text, &outcome);
        self.supervisor.record_chat_child_result(
            session,
            handler,
            &child_thread,
            status,
            &final_text,
            &outcome,
            Some(usage),
            final_answer_ref,
        )?;
        if let Some(warning) = budget_warning {
            let _ = handler.handle(RunEvent::Notice(format!(
                "agent budget warning after child completion: {warning}"
            )));
        }
        Ok(child_thread.thread_id)
    }
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
mod tests {
    use super::*;

    #[test]
    fn spawn_scope_overlap_warning_detects_parent_child_path_overlap() -> Result<()> {
        let mut session = Session::new("parent", "model");
        session.append_user_message(ModelMessage::user(
            "Review crates/sigil-kernel/src/permission.rs and approval flow",
        ))?;
        let parsed = SpawnAgentArgs {
            profile_id: AgentProfileId::new("explore")?,
            objective: "inspect crates/sigil-kernel/src/permission.rs".to_owned(),
            prompt: "read permission implementation".to_owned(),
            mode: AgentInvocationMode::JoinBeforeFinal,
            display_name_hint: None,
        };

        let warning = spawn_scope_overlap_warning(&session, &parsed)
            .expect("path overlap should produce a warning");

        assert!(warning.contains("crates/sigil-kernel/src/permission.rs"));
        Ok(())
    }

    #[test]
    fn spawn_scope_overlap_warning_ignores_unrelated_scopes() -> Result<()> {
        let mut session = Session::new("parent", "model");
        session.append_user_message(ModelMessage::user("Review crates/sigil-tui/src/app.rs"))?;
        let parsed = SpawnAgentArgs {
            profile_id: AgentProfileId::new("explore")?,
            objective: "inspect crates/sigil-kernel/src/permission.rs".to_owned(),
            prompt: "read permission implementation".to_owned(),
            mode: AgentInvocationMode::JoinBeforeFinal,
            display_name_hint: None,
        };

        assert!(spawn_scope_overlap_warning(&session, &parsed).is_none());
        Ok(())
    }
}
