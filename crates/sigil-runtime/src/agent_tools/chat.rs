use super::*;

async fn prepare_background_isolated_write_controls(
    session: &mut Session,
    handler: &mut (dyn EventHandler + Send),
    thread_id: &AgentThreadId,
    final_text: &str,
    outcome: &AgentRunOutcome,
    owner: Option<BackgroundChatAgentWriteOwner>,
) -> Result<Option<PreparedChatIsolatedChildControls>> {
    match owner {
        None => Ok(None),
        Some(BackgroundChatAgentWriteOwner::ChangesetOnly {
            source_observation,
            workspace_root,
        }) => prepare_chat_changeset_only_child_controls(
            session,
            thread_id,
            source_observation,
            final_text,
            outcome,
            &workspace_root,
        )
        .await
        .map(PreparedChatIsolatedChildControls::ChangesetOnly)
        .map(Some),
        Some(BackgroundChatAgentWriteOwner::Worktree {
            worktree,
            workspace_root,
            objective,
        }) => {
            let prepared = prepare_chat_worktree_child_controls(
                session,
                thread_id,
                &worktree,
                outcome,
                &workspace_root,
                &objective,
            )
            .await;
            let cleanup = cleanup_chat_worktree(session, handler, *worktree).await;
            match (prepared, cleanup) {
                (Err(error), _) => Err(error),
                (Ok(_), Err(error)) => Err(error),
                (Ok(None), Ok(())) => Ok(None),
                (Ok(Some(controls)), Ok(())) => {
                    Ok(Some(PreparedChatIsolatedChildControls::Worktree(controls)))
                }
            }
        }
    }
}

pub(super) async fn cleanup_background_isolated_write_owner(
    session: &mut Session,
    handler: &mut (dyn EventHandler + Send),
    owner: BackgroundChatAgentWriteOwner,
) -> Result<()> {
    match owner {
        BackgroundChatAgentWriteOwner::ChangesetOnly { .. } => Ok(()),
        BackgroundChatAgentWriteOwner::Worktree { worktree, .. } => {
            cleanup_chat_worktree(session, handler, *worktree).await
        }
    }
}

pub(super) async fn record_finished_background_run(
    session: &mut Session,
    handler: &mut (dyn EventHandler + Send),
    background: BackgroundChatAgentHandle,
) -> Result<AgentThreadId> {
    let BackgroundChatAgentHandle {
        thread: thread_record,
        handle,
        collection_supervisor,
        write_owner,
        ..
    } = background;
    let supervisor = &collection_supervisor;
    let thread = thread_record.to_runtime_thread();
    let thread_id = thread.thread_id.clone();
    match handle.finish().await {
        Ok(Ok(output)) => {
            supervisor.close_registered_chat_child_user_input_routes(
                session,
                handler,
                &thread_id,
                AgentRouteStatus::Resolved,
            )?;
            let budget_warning = supervisor
                .validate_usage_budget(&thread.budget_scope_id, &output.usage)
                .err()
                .map(|error| format!("{error:#}"));
            match output.disposition {
                BackgroundChatAgentDisposition::Finished {
                    materialized,
                    status,
                } => {
                    let write_controls = match prepare_background_isolated_write_controls(
                        session,
                        handler,
                        &thread_id,
                        &materialized.execution_text,
                        &output.outcome,
                        write_owner,
                    )
                    .await
                    {
                        Ok(controls) => controls,
                        Err(error) => {
                            let reason = format!(
                                "background isolated-write result could not be recorded: {error:#}"
                            );
                            supervisor.record_chat_child_failure(
                                session,
                                handler,
                                &thread,
                                reason.clone(),
                            )?;
                            let _ = handler.handle(RunEvent::Notice(format!(
                                "agent {} failed: {reason}",
                                thread_id.as_str()
                            )));
                            return Ok(thread_id);
                        }
                    };
                    supervisor.record_chat_child_result(
                        session,
                        handler,
                        &thread,
                        status,
                        &materialized,
                        &output.outcome,
                        Some(output.usage),
                    )?;
                    if let Some(controls) = write_controls {
                        append_prepared_chat_isolated_child_controls(session, handler, controls)?;
                    }
                }
                BackgroundChatAgentDisposition::AwaitingUserInput { request } => {
                    if let Some(owner) = write_owner {
                        cleanup_background_isolated_write_owner(session, handler, owner).await?;
                        supervisor.record_chat_child_failure(
                            session,
                            handler,
                            &thread,
                            "isolated-write background child cannot suspend for user input; resume in foreground".to_owned(),
                        )?;
                    } else {
                        supervisor.record_chat_child_waiting_for_input(
                            session, handler, &thread, *request,
                        )?;
                    }
                }
            }
            supervisor.record_chat_mailbox_consumed(
                session,
                handler,
                &thread,
                &output.consumed_mailbox_route_ids,
            )?;
            if let Some(warning) = budget_warning {
                let _ = handler.handle(RunEvent::Notice(format!(
                    "agent budget warning after child completion: {warning}"
                )));
            }
            let _ = handler.handle(RunEvent::Notice(format!(
                "agent {} finished",
                thread_id.as_str()
            )));
        }
        Ok(Err(error)) => {
            supervisor.close_registered_chat_child_user_input_routes(
                session,
                handler,
                &thread_id,
                AgentRouteStatus::Stale,
            )?;
            if let Some(blocked) = error.downcast_ref::<BackgroundApprovalRequired>() {
                if let Some(owner) = write_owner {
                    cleanup_background_isolated_write_owner(session, handler, owner).await?;
                }
                supervisor.record_chat_child_blocked_for_approval(
                    session,
                    handler,
                    &thread,
                    blocked.route(),
                )?;
                let _ = handler.handle(RunEvent::Notice(format!(
                    "agent {} is blocked waiting for approval",
                    thread_id.as_str()
                )));
            } else {
                if let Some(owner) = write_owner {
                    cleanup_background_isolated_write_owner(session, handler, owner).await?;
                }
                let reason = format!("{error:#}");
                supervisor.record_chat_child_failure(session, handler, &thread, reason.clone())?;
                let _ = handler.handle(RunEvent::Notice(format!(
                    "agent {} failed: {reason}",
                    thread_id.as_str()
                )));
            }
        }
        Err(error) => {
            supervisor.close_registered_chat_child_user_input_routes(
                session,
                handler,
                &thread_id,
                AgentRouteStatus::Stale,
            )?;
            let reason = format!("background child agent join failed: {error}");
            if let Some(owner) = write_owner {
                cleanup_background_isolated_write_owner(session, handler, owner).await?;
            }
            supervisor.record_chat_child_failure(session, handler, &thread, reason.clone())?;
            let _ = handler.handle(RunEvent::Notice(format!(
                "agent {} failed: {reason}",
                thread_id.as_str()
            )));
        }
    }
    Ok(thread_id)
}

impl AgentToolRuntime {
    pub(super) async fn wait_agent(
        &mut self,
        session: &mut Session,
        call: &ToolCall,
        args: &Value,
        handler: &mut (dyn EventHandler + Send),
    ) -> ToolResult {
        let thread_id = match thread_id_arg(args) {
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
        if let Some(background) = self.background_runs.remove_if_finished(&thread_id)
            && let Err(error) = record_finished_background_run(session, handler, background).await
        {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::Internal,
                error.to_string(),
            );
        }
        if self.background_runs.contains(&thread_id) {
            if let Some(retry_after) = self.wait_throttle_remaining(&thread_id) {
                let projection = session.agent_thread_state_projection();
                let Some(thread) = projection.threads.get(&thread_id) else {
                    return ToolResult::error(
                        call.id.clone(),
                        call.name.clone(),
                        ToolErrorKind::NotFound,
                        format!("agent thread {} was not found", thread_id.as_str()),
                    );
                };
                return agent_wait_throttled_tool_result(call, thread, retry_after);
            }
            let wait_started = Instant::now();
            loop {
                if let Some(background) = self.background_runs.remove_if_finished(&thread_id) {
                    if let Err(error) =
                        record_finished_background_run(session, handler, background).await
                    {
                        return ToolResult::error(
                            call.id.clone(),
                            call.name.clone(),
                            ToolErrorKind::Internal,
                            error.to_string(),
                        );
                    }
                    break;
                }
                if !self.background_runs.is_running(&thread_id)
                    || saturating_elapsed(wait_started) >= WAIT_AGENT_BACKGROUND_WAIT_TIMEOUT
                {
                    break;
                }
                tokio::time::sleep(WAIT_AGENT_BACKGROUND_POLL_INTERVAL).await;
            }
        }
        if let Some(background) = self.background_runs.remove_if_finished(&thread_id)
            && let Err(error) = record_finished_background_run(session, handler, background).await
        {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::Internal,
                error.to_string(),
            );
        }
        let mut projection = session.agent_thread_state_projection();
        let Some(thread) = projection.threads.get(&thread_id) else {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::NotFound,
                format!("agent thread {} was not found", thread_id.as_str()),
            );
        };
        if !thread.status.is_terminal() && !self.background_runs.contains(&thread_id) {
            let reason =
                "agent runtime handle is unavailable; cannot wait for this thread in the current process"
                    .to_owned();
            let status = ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
                thread_id: thread_id.clone(),
                status: AgentThreadStatus::Unavailable,
                reason: Some(reason),
                updated_at_ms: Some(unix_time_ms()),
            });
            if let Err(error) = handler.commit_controls(session, vec![status]) {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    error.to_string(),
                );
            }
            self.pending_waits.remove(&thread_id);
            projection = session.agent_thread_state_projection();
        }
        let Some(thread) = projection.threads.get(&thread_id) else {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::NotFound,
                format!("agent thread {} was not found", thread_id.as_str()),
            );
        };
        if thread.status.is_terminal() {
            self.pending_waits.remove(&thread_id);
            if let Some(result) = thread
                .result
                .as_ref()
                .filter(|result| agent_result_has_page_source(session, result))
            {
                let offset_chars = self
                    .result_delivery_in_context(session, result)
                    .contiguous_chars();
                let result_args = json!({
                    "thread_id": thread_id.as_str(),
                    "offset_chars": offset_chars,
                    "max_chars": MAX_RESULT_PAGE_LIMIT,
                });
                return self.read_agent_result(session, call, &result_args, handler);
            }
        } else {
            if let Some(retry_after) = self.wait_throttle_remaining(&thread_id) {
                return agent_wait_throttled_tool_result(call, thread, retry_after);
            }
            self.record_pending_wait(&thread_id);
        }
        agent_status_tool_result(session, call, thread)
    }

    fn wait_throttle_remaining(&self, thread_id: &AgentThreadId) -> Option<Duration> {
        let last_wait = self.pending_waits.get(thread_id)?;
        wait_throttle_remaining_since(*last_wait)
    }

    fn record_pending_wait(&mut self, thread_id: &AgentThreadId) {
        self.pending_waits.insert(thread_id.clone(), Instant::now());
    }

    pub(super) fn read_agent_result(
        &mut self,
        session: &mut Session,
        call: &ToolCall,
        args: &Value,
        handler: &mut (dyn EventHandler + Send),
    ) -> ToolResult {
        let thread_id = match thread_id_arg(args) {
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
        let result_page_request = match required_result_page_request_arg(args) {
            Ok(request) => request,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::InvalidInput,
                    error.to_string(),
                );
            }
        };
        let projection = session.agent_thread_state_projection();
        let Some(thread) = projection.threads.get(&thread_id) else {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::NotFound,
                format!("agent thread {} was not found", thread_id.as_str()),
            );
        };
        let Some(result) = thread.result.as_ref() else {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::Unsupported,
                format!(
                    "agent thread {} has no terminal result yet",
                    thread_id.as_str()
                ),
            );
        };
        let mut coverage = self.result_delivery_in_context(session, result);
        if let Some(already_delivered) = agent_result_page_already_delivered_tool_result(
            session,
            call,
            result,
            &result_page_request,
            &coverage,
        ) {
            return already_delivered;
        }
        let result_page = match read_agent_result_page(session, result, result_page_request) {
            Ok(page) => page,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    error.to_string(),
                );
            }
        };
        let delivery = AgentThreadResultDeliveredEntry {
            thread_id: result.thread_id.clone(),
            call_id: call.id.clone(),
            output_hash: result.output_hash.clone(),
            offset_chars: result_page.offset_chars,
            returned_chars: result_page.returned_chars,
            total_chars: result_page.total_chars,
            truncated: result_page.truncated,
            delivered_at_ms: None,
        };
        if let Err(error) = handler.commit_controls(
            session,
            vec![ControlEntry::AgentThreadResultDelivered(delivery.clone())],
        ) {
            // A publication error can follow a successful append. No body is returned on this
            // path, so conservatively restart this session's transient delivery accounting.
            self.result_context_frontiers.insert(
                session.session_scope_id().to_owned(),
                session.entries().len(),
            );
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::Internal,
                error.to_string(),
            );
        }
        coverage.record(&delivery);
        agent_result_page_tool_result(call, result, &result_page, &coverage)
    }

    pub(super) fn result_delivery_in_context(
        &mut self,
        session: &Session,
        result: &AgentThreadResult,
    ) -> AgentResultDeliveryCoverage {
        let entries = session.entries();
        let frontier = self
            .result_context_frontiers
            .entry(session.session_scope_id().to_owned())
            .or_insert(entries.len());
        // Reloading an earlier log frontier cannot retain delivery from the discarded tail.
        *frontier = (*frontier).min(entries.len());
        session.agent_result_delivery_since(&result.thread_id, &result.output_hash, *frontier)
    }

    pub(super) fn list_agents(&self, session: &Session, call: &ToolCall) -> ToolResult {
        let projection = session.agent_thread_state_projection();
        let agents = projection
            .threads
            .values()
            .map(|thread| {
                let result_ref = thread.result.as_ref().map(|result| {
                    let page_available = agent_result_has_page_source(session, result);
                    json!({
                        "thread_id": result.thread_id.as_str(),
                        "status": terminal_status_label(result.status),
                        "summary_truncated": result.summary_truncated,
                        "original_summary_chars": result.original_summary_chars,
                        "changed_paths_count": result.changed_paths.len(),
                        "artifact_count": result.artifacts.len(),
                        "page_available": page_available,
                        "read_tool": page_available.then_some(READ_AGENT_RESULT_TOOL_NAME),
                        "read_args": page_available.then(|| json!({
                            "thread_id": result.thread_id.as_str(),
                            "offset_chars": 0,
                            "max_chars": MAX_RESULT_PAGE_LIMIT,
                        }))
                    })
                });
                let approval_pending = projection.approval_routes.values().any(|route| {
                    route.source_thread_id == thread.thread_id
                        && route.status == AgentRouteStatus::Requested
                });
                let background_handle_available = self.background_runs.contains(&thread.thread_id);
                json!({
                    "thread_id": thread.thread_id.as_str(),
                    "display_name": thread.display_name.as_deref(),
                    "profile_id": thread.profile_id.as_ref().map(AgentProfileId::as_str),
                    "mode": thread.invocation_mode.map(invocation_mode_label),
                    "status": thread_status_label(thread.status),
                    "terminal": thread.status.is_terminal(),
                    "objective": thread.objective,
                    "messageable": !thread.status.is_terminal() && background_handle_available,
                    "closable": thread.status.is_terminal() && !thread.closed,
                    "cancelable": !thread.status.is_terminal() && background_handle_available,
                    "approval_pending": approval_pending,
                    "result_ref": result_ref,
                })
            })
            .collect::<Vec<_>>();
        let count = agents.len();
        ToolResult::ok(
            call.id.clone(),
            call.name.clone(),
            serde_json::to_string(&json!({
                "agents": agents,
                "count": count,
            }))
            .unwrap_or_else(|error| format!("failed to serialize agent list: {error}")),
            ToolResultMeta {
                details: json!({
                    "count": count,
                }),
                ..ToolResultMeta::default()
            },
        )
    }

    pub(super) async fn cancel_agent(
        &mut self,
        session: &mut Session,
        call: &ToolCall,
        args: &Value,
        handler: &mut (dyn EventHandler + Send),
    ) -> ToolResult {
        let thread_id = match thread_id_arg(args) {
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
        let reason = optional_string(args, "reason")
            .unwrap_or_else(|| "agent cancelled by request".to_owned());
        let projection = session.agent_thread_state_projection();
        let Some(thread) = projection.threads.get(&thread_id) else {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::NotFound,
                format!("agent thread {} was not found", thread_id.as_str()),
            );
        };
        let previous_status = thread.status;
        if previous_status.is_terminal() {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::Unsupported,
                format!(
                    "agent thread {} is already {}",
                    thread_id.as_str(),
                    thread_status_label(previous_status)
                ),
            );
        }
        let cancellation = match self
            .background_runs
            .cancel_agent_thread_durably(session, &thread_id, reason, handler)
            .await
        {
            Ok(Some(cancellation)) => cancellation,
            Ok(None) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Unsupported,
                    format!(
                        "agent thread {} has no cancellable runtime handle",
                        thread_id.as_str()
                    ),
                );
            }
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    error.to_string(),
                );
            }
        };
        ToolResult::ok(
            call.id.clone(),
            call.name.clone(),
            serde_json::to_string(&json!({
                "thread_id": thread_id.as_str(),
                "previous_status": thread_status_label(cancellation.previous_status),
                "status": cancellation.status_label,
                "reason": cancellation.reason,
                "cleanup_complete": cancellation.cleanup_complete,
                "next_action": "do not wait for this agent; report the durable terminal status"
            }))
            .unwrap_or_else(|error| format!("failed to serialize agent cancel result: {error}")),
            ToolResultMeta {
                details: json!({
                    "thread_id": thread_id.as_str(),
                    "previous_status": thread_status_label(cancellation.previous_status),
                    "status": thread_status_label(cancellation.status),
                    "cleanup_complete": cancellation.cleanup_complete,
                }),
                ..ToolResultMeta::default()
            },
        )
    }

    pub(super) fn message_agent(
        &self,
        session: &Session,
        call: &ToolCall,
        args: &Value,
    ) -> ToolResult {
        let thread_id = match thread_id_arg(args) {
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
        let prompt = match required_string(args, "prompt") {
            Ok(prompt) => prompt,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::InvalidInput,
                    error.to_string(),
                );
            }
        };
        let projection = session.agent_thread_state_projection();
        let Some(thread) = projection.threads.get(&thread_id) else {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::NotFound,
                format!("agent thread {} was not found", thread_id.as_str()),
            );
        };
        let route_id = match agent_route_id_for_call(&thread_id, &call.id) {
            Ok(route_id) => route_id,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    error.to_string(),
                );
            }
        };
        let source_thread_id = match AgentThreadId::new(MAIN_THREAD_ID) {
            Ok(thread_id) => thread_id,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    error.to_string(),
                );
            }
        };
        let safe_prompt = sigil_kernel::safe_persistence_text(&prompt);
        let prompt_hash = hash_text(&safe_prompt);
        let requested = AgentThreadMessageRoutedEntry {
            route_id: route_id.clone(),
            source_thread_id: source_thread_id.clone(),
            target_thread_id: thread_id.clone(),
            prompt_hash: prompt_hash.clone(),
            prompt: Some(safe_prompt.clone()),
            status: AgentRouteStatus::Requested,
        };
        let mailbox_queued = AgentMailboxMessageEntry {
            route_id: route_id.clone(),
            source_thread_id: source_thread_id.clone(),
            target_thread_id: thread_id.clone(),
            prompt_hash: prompt_hash.clone(),
            prompt: Some(safe_prompt),
            status: AgentMailboxStatus::Queued,
            reason: None,
            updated_at_ms: None,
        };
        let delivery = if thread.status.is_terminal() {
            Err(format!(
                "agent thread {} is {}",
                thread_id.as_str(),
                thread_status_label(thread.status)
            ))
        } else {
            self.supervisor.send_agent_message(
                &thread_id,
                AgentMailboxMessage {
                    route_id: route_id.clone(),
                    prompt: prompt.clone(),
                },
            )
        };
        match delivery {
            Ok(()) => ToolResult::ok(
                call.id.clone(),
                call.name.clone(),
                serde_json::to_string(&json!({
                    "thread_id": thread_id.as_str(),
                    "route_id": route_id.as_str(),
                    "status": "resolved",
                    "delivery": "delivered_to_mailbox",
                    "delivered_to_mailbox": true,
                    "safe_point": "after_current_turn",
                    "will_apply_after_current_turn": true,
                    "interrupt_requested": false,
                    "interrupts_in_flight_provider_stream": false,
                    "next_action": "call wait_agent to collect terminal results; the child applies this message at its next safe point"
                }))
                .unwrap_or_else(|error| format!("failed to serialize agent message route: {error}")),
                ToolResultMeta {
                    details: json!({
                        "thread_id": thread_id.as_str(),
                        "route_id": route_id.as_str(),
                        "status": "resolved",
                        "delivery": "delivered_to_mailbox",
                        "delivered_to_mailbox": true,
                        "safe_point": "after_current_turn",
                        "will_apply_after_current_turn": true,
                        "interrupt_requested": false,
                        "interrupts_in_flight_provider_stream": false
                    }),
                    ..ToolResultMeta::default()
                },
            )
            .with_control_entry(ControlEntry::AgentThreadMessageRouted(requested))
            .with_control_entry(ControlEntry::AgentMailboxMessage(mailbox_queued))
            .with_control_entry(ControlEntry::AgentMailboxMessage(
                AgentMailboxMessageEntry {
                    route_id: route_id.clone(),
                    source_thread_id: source_thread_id.clone(),
                    target_thread_id: thread_id.clone(),
                    prompt_hash: String::new(),
                    prompt: None,
                    status: AgentMailboxStatus::Delivered,
                    reason: None,
                    updated_at_ms: None,
                },
            ))
            .with_control_entry(ControlEntry::AgentThreadMessageRouted(
                AgentThreadMessageRoutedEntry {
                    route_id,
                    source_thread_id,
                    target_thread_id: thread_id,
                    prompt_hash,
                    prompt: None,
                    status: AgentRouteStatus::Resolved,
                },
            )),
            Err(reason) => ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::Unsupported,
                format!(
                    "agent thread {} cannot accept safe-point messages: {}",
                    thread_id.as_str(),
                    reason
                ),
            )
            .with_control_entry(ControlEntry::AgentThreadMessageRouted(requested))
            .with_control_entry(ControlEntry::AgentMailboxMessage(mailbox_queued))
            .with_control_entry(ControlEntry::AgentMailboxMessage(
                AgentMailboxMessageEntry {
                    route_id: route_id.clone(),
                    source_thread_id: source_thread_id.clone(),
                    target_thread_id: thread_id.clone(),
                    prompt_hash: String::new(),
                    prompt: None,
                    status: AgentMailboxStatus::Rejected,
                    reason: Some(reason),
                    updated_at_ms: None,
                },
            ))
            .with_control_entry(ControlEntry::AgentThreadMessageRouted(
                AgentThreadMessageRoutedEntry {
                    route_id,
                    source_thread_id,
                    target_thread_id: thread_id,
                    prompt_hash,
                    prompt: None,
                    status: AgentRouteStatus::Rejected,
                },
            )),
        }
    }

    pub(super) fn close_agent(
        &self,
        session: &Session,
        call: &ToolCall,
        args: &Value,
    ) -> ToolResult {
        close_agent_from_args(session, call, args)
    }
}

pub(super) fn wait_throttle_remaining_since(last_wait: Instant) -> Option<Duration> {
    wait_throttle_remaining_for_elapsed(saturating_elapsed(last_wait))
}

pub(super) fn wait_throttle_remaining_for_elapsed(elapsed: Duration) -> Option<Duration> {
    WAIT_AGENT_MIN_REPOLL_INTERVAL
        .checked_sub(elapsed)
        .filter(|remaining| !remaining.is_zero())
}

pub(super) fn close_agent_from_args(
    session: &Session,
    call: &ToolCall,
    args: &Value,
) -> ToolResult {
    let thread_id = match thread_id_arg(args) {
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
    let projection = session.agent_thread_state_projection();
    let Some(thread) = projection.threads.get(&thread_id) else {
        return ToolResult::error(
            call.id.clone(),
            call.name.clone(),
            ToolErrorKind::NotFound,
            format!("agent thread {} was not found", thread_id.as_str()),
        );
    };
    if !thread.status.is_terminal() {
        return ToolResult::error(
            call.id.clone(),
            call.name.clone(),
            ToolErrorKind::Unsupported,
            format!(
                "agent thread {} is {}; close_agent only closes terminal threads",
                thread_id.as_str(),
                thread_status_label(thread.status)
            ),
        );
    }
    let reason = optional_string(args, "reason");
    ToolResult::ok(
        call.id.clone(),
        call.name.clone(),
        format!("agent thread {} closed", thread_id.as_str()),
        ToolResultMeta::default(),
    )
    .with_control_entry(ControlEntry::AgentThreadClosed(AgentThreadClosedEntry {
        thread_id,
        reason,
    }))
}
