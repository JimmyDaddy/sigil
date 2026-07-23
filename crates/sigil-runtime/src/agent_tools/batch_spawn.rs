use super::spawn::{
    child_tool_registry_for_profile, profile_uses_changeset_only_write, spawn_scope_overlap_warning,
};
use super::*;

struct PreparedBatchSpawnMember {
    request_key: AgentRouteId,
    batch_id: AgentBatchId,
    call: ToolCall,
    start: crate::AgentChatChildStart,
    child_agent: Agent<Box<dyn Provider>>,
    child_session: Session,
    child_input: sigil_kernel::AgentRunInput,
    child_options: sigil_kernel::AgentRunOptions,
}

struct StartedBatchSpawnMember {
    request_key: AgentRouteId,
    batch_id: AgentBatchId,
    call: ToolCall,
    child_thread: crate::AgentChatChildThread,
    child_agent: Agent<Box<dyn Provider>>,
    child_session: Session,
    child_input: sigil_kernel::AgentRunInput,
    child_options: sigil_kernel::AgentRunOptions,
}

impl AgentToolRuntime {
    pub(super) fn spawn_agents(
        &mut self,
        session: &mut Session,
        call: &ToolCall,
        args: &Value,
        options: &sigil_kernel::AgentRunOptions,
        handler: &mut (dyn EventHandler + Send),
    ) -> ToolResult {
        let parsed = match SpawnAgentsArgs::parse(args) {
            Ok(parsed) => parsed,
            Err(error) => {
                return batch_spawn_error(
                    call,
                    ToolErrorKind::InvalidInput,
                    None,
                    format!("{error:#}"),
                );
            }
        };
        let completion_mode = parsed.completion_mode;
        if completion_mode == AgentInvocationMode::JoinBeforeFinal
            && (!self.join_batch_eligible || self.run_cancellation.is_none())
        {
            return batch_spawn_error(
                call,
                ToolErrorKind::Unsupported,
                None,
                "spawn_agents requires a host-owned root-run join barrier".to_owned(),
            );
        }
        if completion_mode == AgentInvocationMode::JoinBeforeFinal
            && self
                .run_cancellation
                .as_ref()
                .is_some_and(sigil_kernel::RunCancellationHandle::is_cancel_requested)
        {
            return batch_spawn_error(
                call,
                ToolErrorKind::Interrupted,
                None,
                "root run cancelled before batch spawn admission".to_owned(),
            );
        }

        let batch_id = match self
            .root_logical_run_id
            .as_deref()
            .ok_or_else(|| anyhow!("spawn_agents is missing the root logical-run identity"))
            .and_then(|logical_run_id| agent_batch_id(logical_run_id, &call.id))
        {
            Ok(batch_id) => batch_id,
            Err(error) => {
                return batch_spawn_error(
                    call,
                    ToolErrorKind::Internal,
                    None,
                    format!("{error:#}"),
                );
            }
        };
        let parent_session_ref = match parent_session_ref(session) {
            Ok(reference) => reference,
            Err(error) => {
                return batch_spawn_error(
                    call,
                    ToolErrorKind::Internal,
                    None,
                    format!("{error:#}"),
                );
            }
        };
        let parent_thread_id = match AgentThreadId::new(MAIN_THREAD_ID) {
            Ok(thread_id) => thread_id,
            Err(error) => {
                return batch_spawn_error(
                    call,
                    ToolErrorKind::Internal,
                    None,
                    format!("{error:#}"),
                );
            }
        };
        let authority = self.model_delegation_authority();
        let mut prepared = Vec::with_capacity(parsed.members.len());
        for member in parsed.members {
            let request_key = member.request_key;
            let member_call = ToolCall {
                id: batch_spawn_member_call_id(&batch_id, &request_key),
                name: SPAWN_AGENT_TOOL_NAME.to_owned(),
                args_json: member.raw_args.to_string(),
            };
            let spawn = member.spawn;
            let resolved_profile = match self.resolve_spawn_profile(&spawn.profile_id) {
                Ok(profile) => profile,
                Err(error) => {
                    return batch_spawn_error(
                        call,
                        ToolErrorKind::PermissionDenied,
                        Some(&request_key),
                        format!("{error:#}"),
                    );
                }
            };
            let role = resolved_profile.execution_role;
            if profile_uses_changeset_only_write(role, &resolved_profile) {
                return batch_spawn_error(
                    call,
                    ToolErrorKind::PermissionDenied,
                    Some(&request_key),
                    "spawn_agents only accepts proven read-only participants".to_owned(),
                );
            }
            let child_registry = child_tool_registry_for_profile(
                &self.base_registry,
                &self.root_config,
                role,
                false,
                resolved_profile.profile.tool_scope.clone(),
            );
            if let Err(error) = admit_model_agent_spawn(
                self.root_config.task.multi_agent_mode,
                &authority,
                &resolved_profile,
                &child_registry,
            ) {
                return batch_spawn_error(
                    call,
                    ToolErrorKind::PermissionDenied,
                    Some(&request_key),
                    format!("{error:#}"),
                );
            }
            if !tool_registry_is_safe_readonly_for_auto_spawn(&child_registry) {
                return batch_spawn_error(
                    call,
                    ToolErrorKind::PermissionDenied,
                    Some(&request_key),
                    "spawn_agents member tool contracts are not proven read-only".to_owned(),
                );
            }
            let thread_id = match chat_agent_thread_id_for_call(&member_call.id, &spawn.profile_id)
            {
                Ok(thread_id) => thread_id,
                Err(error) => {
                    return batch_spawn_error(
                        call,
                        ToolErrorKind::InvalidInput,
                        Some(&request_key),
                        format!("{error:#}"),
                    );
                }
            };
            let delegation_admission = match delegation_admission_entry(
                authority.clone(),
                thread_id.clone(),
                spawn.profile_id.clone(),
                completion_mode,
                AgentInvocationSource::Chat,
                &spawn.objective,
                &child_registry,
            ) {
                Ok(admission) => admission,
                Err(error) => {
                    return batch_spawn_error(
                        call,
                        ToolErrorKind::Internal,
                        Some(&request_key),
                        format!("{error:#}"),
                    );
                }
            };
            let child_provider = match self.provider_factory.build_provider(
                &self.root_config,
                role,
                &spawn.profile_id,
            ) {
                Ok(provider) => provider,
                Err(error) => {
                    return batch_spawn_error(
                        call,
                        ToolErrorKind::Internal,
                        Some(&request_key),
                        format!("failed to build child agent provider: {error:#}"),
                    );
                }
            };
            let child_session_ref = match agent_child_session_ref(&thread_id) {
                Ok(reference) => reference,
                Err(error) => {
                    return batch_spawn_error(
                        call,
                        ToolErrorKind::Internal,
                        Some(&request_key),
                        format!("{error:#}"),
                    );
                }
            };
            let budget_scope_id = match chat_budget_scope_id(&member_call.id) {
                Ok(task_id) => task_id,
                Err(error) => {
                    return batch_spawn_error(
                        call,
                        ToolErrorKind::Internal,
                        Some(&request_key),
                        format!("{error:#}"),
                    );
                }
            };
            let child_capabilities = child_provider.capabilities();
            let child_session = match build_agent_child_session(session, &child_session_ref) {
                Ok(child_session) => child_session,
                Err(error) => {
                    return batch_spawn_error(
                        call,
                        ToolErrorKind::Internal,
                        Some(&request_key),
                        format!("{error:#}"),
                    );
                }
            };
            let mut child_messages = Vec::new();
            if let Some(system_prompt) = agent_profile_system_prompt(&resolved_profile) {
                child_messages.push(ModelMessage::system(system_prompt));
            }
            child_messages.push(ModelMessage::user(spawn.prompt.clone()));
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
            if let Some(warning) = spawn_scope_overlap_warning(session, &spawn) {
                let _ = handler.handle(RunEvent::Notice(warning));
            }
            let start = crate::AgentChatChildStart {
                call_id: member_call.id.clone(),
                budget_scope_id,
                parent_thread_id: parent_thread_id.clone(),
                parent_depth: 0,
                batch_id: Some(batch_id.clone()),
                batch_member_key: Some(request_key.clone()),
                parent_session_ref: parent_session_ref.clone(),
                profile_id: spawn.profile_id,
                role,
                child_session_ref,
                objective: spawn.objective,
                prompt: spawn.prompt,
                workspace_root: options.workspace_root.clone(),
                provider_capabilities: child_capabilities,
                invocation_mode: completion_mode,
                invocation_source: AgentInvocationSource::Chat,
                delegation_admission,
                display_name_hint: spawn.display_name_hint,
            };
            prepared.push(PreparedBatchSpawnMember {
                request_key,
                batch_id: batch_id.clone(),
                call: member_call,
                start,
                child_agent: Agent::new(child_provider, child_registry),
                child_session,
                child_input,
                child_options,
            });
        }

        let starts = prepared
            .iter()
            .map(|member| member.start.clone())
            .collect::<Vec<_>>();
        let reservation = match self.supervisor.reserve_chat_child_batch(&starts) {
            Ok(reservation) => reservation,
            Err(error) => {
                return batch_spawn_error(
                    call,
                    ToolErrorKind::PermissionDenied,
                    None,
                    format!("{error:#}"),
                );
            }
        };
        let mut started: Vec<StartedBatchSpawnMember> = Vec::with_capacity(prepared.len());
        for member in prepared {
            let child_thread =
                match self
                    .supervisor
                    .begin_chat_child_thread(session, handler, member.start)
                {
                    Ok(child_thread) => child_thread,
                    Err(error) => {
                        for started_member in &started {
                            let _ = self.supervisor.record_chat_child_failure(
                                session,
                                handler,
                                &started_member.child_thread,
                                "batch start rolled back before provider dispatch".to_owned(),
                            );
                        }
                        return batch_spawn_error(
                            call,
                            ToolErrorKind::Internal,
                            Some(&member.request_key),
                            format!("failed to commit child start: {error:#}"),
                        );
                    }
                };
            started.push(StartedBatchSpawnMember {
                request_key: member.request_key,
                batch_id: member.batch_id,
                call: member.call,
                child_thread,
                child_agent: member.child_agent,
                child_session: member.child_session,
                child_input: member.child_input,
                child_options: member.child_options,
            });
        }
        reservation.commit();

        if completion_mode == AgentInvocationMode::Background {
            return self.start_background_agent_batch(session, call, handler, &batch_id, started);
        }

        let mut joined_members = Vec::with_capacity(started.len());
        let mut remaining = started.into_iter();
        while let Some(member) = remaining.next() {
            let thread_id = member.child_thread.thread_id.clone();
            let registration = self.start_joined_chat_child(
                session,
                &member.call,
                member.child_thread,
                member.child_agent,
                member.child_session,
                member.child_input,
                member.child_options,
                handler,
                Some(AgentBatchMemberContext {
                    batch_id: member.batch_id.as_str().to_owned(),
                    request_key: member.request_key.as_str().to_owned(),
                }),
            );
            if registration.is_error() {
                let registration_error = registration
                    .summary()
                    .error_message
                    .unwrap_or_else(|| "joined child registration failed".to_owned());
                let abort_error = self
                    .abort_current_join_dependencies(
                        session,
                        handler,
                        "spawn_agents registration failed before host join settle",
                    )
                    .err();
                for pending in remaining {
                    let _ = self.supervisor.record_chat_child_failure(
                        session,
                        handler,
                        &pending.child_thread,
                        "batch registration rolled back before provider dispatch".to_owned(),
                    );
                }
                let message = match abort_error {
                    Some(error) => {
                        format!("{registration_error}; batch cleanup failed: {error:#}")
                    }
                    None => registration_error,
                };
                return batch_spawn_error(
                    call,
                    ToolErrorKind::Internal,
                    Some(&member.request_key),
                    message,
                );
            }
            joined_members.push(json!({
                "request_key": member.request_key.as_str(),
                "batch_id": member.batch_id.as_str(),
                "thread_id": thread_id.as_str(),
                "status": "running",
            }));
        }

        ToolResult::ok(
            call.id.clone(),
            call.name.clone(),
            serde_json::to_string(&json!({
                "status": "running",
                "terminal": false,
                "host_join_registered": true,
                "batch_id": batch_id.as_str(),
                "member_count": joined_members.len(),
                "members": joined_members,
                "next_action": "the host will join every member before the next parent model turn",
                "do_not_call_wait_agent": true,
            }))
            .unwrap_or_else(|error| format!("failed to serialize batch agent status: {error}")),
            ToolResultMeta {
                details: json!({
                    "status": "running",
                    "terminal": false,
                    "host_join_registered": true,
                    "batch_id": batch_id.as_str(),
                    "member_count": joined_members.len(),
                    "members": joined_members,
                }),
                ..ToolResultMeta::default()
            },
        )
    }

    fn start_background_agent_batch(
        &mut self,
        session: &mut Session,
        call: &ToolCall,
        handler: &mut (dyn EventHandler + Send),
        batch_id: &AgentBatchId,
        started: Vec<StartedBatchSpawnMember>,
    ) -> ToolResult {
        if let Some(missing) = started
            .iter()
            .find(|member| member.child_thread.mailbox_rx.is_none())
        {
            let request_key = missing.request_key.clone();
            let mut first_cleanup_error = None;
            for member in &started {
                if let Err(error) = self.supervisor.record_chat_child_failure(
                    session,
                    handler,
                    &member.child_thread,
                    "background batch mailbox was not created".to_owned(),
                ) {
                    self.supervisor
                        .release_runtime_thread(&member.child_thread.thread_id);
                    if first_cleanup_error.is_none() {
                        first_cleanup_error = Some(error);
                    }
                }
            }
            let message = first_cleanup_error.map_or_else(
                || "background batch mailbox was not created".to_owned(),
                |error| {
                    format!("background batch mailbox was not created; cleanup failed: {error:#}")
                },
            );
            return batch_spawn_error(call, ToolErrorKind::Internal, Some(&request_key), message);
        }

        let mut registrations = Vec::with_capacity(started.len());
        let mut start_gates = Vec::with_capacity(started.len());
        let mut background_members = Vec::with_capacity(started.len());
        for member in started {
            let StartedBatchSpawnMember {
                request_key,
                batch_id: member_batch_id,
                call: _,
                mut child_thread,
                child_agent,
                child_session,
                child_input,
                child_options,
            } = member;
            let mailbox_rx = child_thread
                .mailbox_rx
                .take()
                .expect("background batch mailboxes were preflighted");
            let thread_id = child_thread.thread_id.clone();
            let thread_record = BackgroundChatAgentThreadRecord::from_thread(&child_thread);
            let cancellation_owner = RunCancellationOwner::new();
            let cancellation_handle = cancellation_owner.handle();
            let cancellation_task_guard = cancellation_handle
                .register_task()
                .expect("new background batch cancellation owner must admit its first task");
            let child_input = child_input.with_cancellation(cancellation_handle);
            let run_thread_id = thread_id.clone();
            let child_session_ref = child_thread.child_session_ref.clone();
            let event_sink = self.background_runs.event_sink();
            let (start_tx, start_rx) = tokio::sync::oneshot::channel();
            let handle = tokio::spawn(async move {
                let _cancellation_task_guard = cancellation_task_guard;
                start_rx
                    .await
                    .map_err(|_| anyhow!("background batch start gate was cancelled"))?;
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
            registrations.push((
                thread_id.clone(),
                BackgroundChatAgentHandle {
                    thread: thread_record,
                    handle,
                    cancellation_owner,
                },
            ));
            start_gates.push(start_tx);
            background_members.push(json!({
                "request_key": request_key.as_str(),
                "batch_id": member_batch_id.as_str(),
                "thread_id": thread_id.as_str(),
                "status": "running",
            }));
        }

        if let Err(error) = self.background_runs.insert_batch(&mut registrations) {
            drop(start_gates);
            let mut first_cleanup_error = None;
            for (_, background) in &registrations {
                background.handle.abort();
                let thread = background.thread.to_runtime_thread();
                if let Err(cleanup_error) = self.supervisor.record_chat_child_failure(
                    session,
                    handler,
                    &thread,
                    "background batch registration failed before provider dispatch".to_owned(),
                ) {
                    self.supervisor.release_runtime_thread(&thread.thread_id);
                    if first_cleanup_error.is_none() {
                        first_cleanup_error = Some(cleanup_error);
                    }
                }
            }
            let message = first_cleanup_error.map_or_else(
                || format!("{error:#}"),
                |cleanup_error| format!("{error:#}; batch cleanup failed: {cleanup_error:#}"),
            );
            return batch_spawn_error(call, ToolErrorKind::Internal, None, message);
        }
        for start_gate in start_gates {
            let _ = start_gate.send(());
        }

        ToolResult::ok(
            call.id.clone(),
            call.name.clone(),
            serde_json::to_string(&json!({
                "status": "running",
                "terminal": false,
                "backgrounded": true,
                "host_join_registered": false,
                "batch_id": batch_id.as_str(),
                "member_count": background_members.len(),
                "members": background_members,
                "next_action": "continue only non-overlapping parent work; the host will surface result-ready completion without polling",
                "do_not_poll": true,
                "do_not_describe_as_finished": true,
            }))
            .unwrap_or_else(|error| format!("failed to serialize background batch status: {error}")),
            ToolResultMeta {
                details: json!({
                    "status": "running",
                    "terminal": false,
                    "backgrounded": true,
                    "host_join_registered": false,
                    "batch_id": batch_id.as_str(),
                    "member_count": background_members.len(),
                    "members": background_members,
                }),
                ..ToolResultMeta::default()
            },
        )
    }
}

fn agent_batch_id(root_logical_run_id: &str, call_id: &str) -> Result<AgentBatchId> {
    if root_logical_run_id.trim().is_empty() {
        bail!("spawn_agents root logical-run identity is empty");
    }
    AgentBatchId::new(format!(
        "batch_{}",
        short_digest(&hash_text(&format!("{root_logical_run_id}:{call_id}")))
    ))
}

fn batch_spawn_member_call_id(batch_id: &AgentBatchId, request_key: &AgentRouteId) -> String {
    format!("{}-member-{}", batch_id.as_str(), request_key.as_str())
}

fn batch_spawn_error(
    call: &ToolCall,
    kind: ToolErrorKind,
    request_key: Option<&AgentRouteId>,
    message: String,
) -> ToolResult {
    ToolResult::error(call.id.clone(), call.name.clone(), kind, message.clone()).with_error_details(
        false,
        json!({
            "error": "agent_batch_rejected",
            "message": message,
            "request_key": request_key.map(AgentRouteId::as_str),
            "provider_started": false,
            "whole_batch_rejected": true,
        }),
    )
}

#[cfg(test)]
#[path = "tests/batch_spawn_tests.rs"]
mod tests;
