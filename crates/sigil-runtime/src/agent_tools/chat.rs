use super::*;

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
            && let Err(error) = self
                .record_finished_background_run(session, handler, background)
                .await
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
                    if let Err(error) = self
                        .record_finished_background_run(session, handler, background)
                        .await
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
            && let Err(error) = self
                .record_finished_background_run(session, handler, background)
                .await
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
            if let Err(error) = session
                .append_control(status.clone())
                .and_then(|()| handler.handle(RunEvent::Control(status)))
            {
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
        } else {
            if let Some(retry_after) = self.wait_throttle_remaining(&thread_id) {
                return agent_wait_throttled_tool_result(call, thread, retry_after);
            }
            self.record_pending_wait(&thread_id);
        }
        agent_status_tool_result(call, thread)
    }

    fn wait_throttle_remaining(&self, thread_id: &AgentThreadId) -> Option<Duration> {
        let last_wait = self.pending_waits.get(thread_id)?;
        wait_throttle_remaining_since(*last_wait)
    }

    fn record_pending_wait(&mut self, thread_id: &AgentThreadId) {
        self.pending_waits.insert(thread_id.clone(), Instant::now());
    }

    pub(super) async fn record_finished_background_run(
        &mut self,
        session: &mut Session,
        handler: &mut (dyn EventHandler + Send),
        background: BackgroundChatAgentHandle,
    ) -> Result<AgentThreadId> {
        let thread = background.thread.to_runtime_thread();
        let thread_id = thread.thread_id.clone();
        match background.handle.await {
            Ok(Ok(output)) => {
                let budget_warning = self
                    .supervisor
                    .validate_usage_budget(&thread.budget_scope_id, &output.usage)
                    .err()
                    .map(|error| format!("{error:#}"));
                self.supervisor.record_chat_child_result(
                    session,
                    handler,
                    &thread,
                    output.status,
                    &output.materialized,
                    &output.outcome,
                    Some(output.usage),
                )?;
                self.supervisor.record_chat_mailbox_consumed(
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
                let reason = format!("{error:#}");
                self.supervisor.record_chat_child_failure(
                    session,
                    handler,
                    &thread,
                    reason.clone(),
                )?;
                let _ = handler.handle(RunEvent::Notice(format!(
                    "agent {} failed: {reason}",
                    thread_id.as_str()
                )));
            }
            Err(error) => {
                let reason = format!("background child agent join failed: {error}");
                self.supervisor.record_chat_child_failure(
                    session,
                    handler,
                    &thread,
                    reason.clone(),
                )?;
                let _ = handler.handle(RunEvent::Notice(format!(
                    "agent {} failed: {reason}",
                    thread_id.as_str()
                )));
            }
        }
        Ok(thread_id)
    }

    pub(super) fn read_agent_result(
        &self,
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
        if let Some(delivered) =
            full_agent_result_delivery(session, &result.thread_id, &result.output_hash)
        {
            return agent_result_already_delivered_tool_result(call, result, &delivered);
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
        let delivery = ControlEntry::AgentThreadResultDelivered(AgentThreadResultDeliveredEntry {
            thread_id: result.thread_id.clone(),
            call_id: call.id.clone(),
            output_hash: result.output_hash.clone(),
            offset_chars: result_page.offset_chars,
            returned_chars: result_page.returned_chars,
            total_chars: result_page.total_chars,
            truncated: result_page.truncated,
            delivered_at_ms: None,
        });
        if let Err(error) = session
            .append_control(delivery.clone())
            .and_then(|()| handler.handle(RunEvent::Control(delivery)))
        {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::Internal,
                error.to_string(),
            );
        }
        agent_result_page_tool_result(call, result, &result_page)
    }

    pub(super) fn list_agents(&self, session: &Session, call: &ToolCall) -> ToolResult {
        let projection = session.agent_thread_state_projection();
        let agents = projection
            .threads
            .values()
            .map(|thread| {
                let result_ref = thread.result.as_ref().map(|result| {
                    json!({
                        "thread_id": result.thread_id.as_str(),
                        "status": terminal_status_label(result.status),
                        "summary_truncated": result.summary_truncated,
                        "original_summary_chars": result.original_summary_chars,
                        "changed_paths_count": result.changed_paths.len(),
                        "artifact_count": result.artifacts.len(),
                        "read_tool": READ_AGENT_RESULT_TOOL_NAME,
                        "read_args": {
                            "thread_id": result.thread_id.as_str(),
                            "offset_chars": 0,
                            "max_chars": MAX_RESULT_PAGE_LIMIT,
                        }
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
        const QUIESCENCE_TIMEOUT: Duration = Duration::from_secs(5);
        let recorder = match session.run_cancellation_recorder() {
            Ok(recorder) => recorder,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    error.to_string(),
                );
            }
        };
        let run_scope_id = match self.background_runs.reserve_cancellation_scope(&thread_id) {
            Ok(Some(run_scope_id)) => run_scope_id,
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
        let request_id = format!("cancel-{run_scope_id}");
        let requested_at_ms = unix_time_ms();
        if let Err(error) = recorder.append_requested(&RunCancellationRequestedEntry {
            request_id: request_id.clone(),
            run_scope_id: run_scope_id.clone(),
            target: RunCancellationTarget::AgentThread {
                thread_id: thread_id.as_str().to_owned(),
            },
            reason: reason.clone(),
            requested_at_ms,
            quiescence_deadline_ms: requested_at_ms
                .saturating_add(QUIESCENCE_TIMEOUT.as_millis() as u64),
        }) {
            let _ = self
                .background_runs
                .cancel(&thread_id, QUIESCENCE_TIMEOUT)
                .await;
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::Internal,
                error.to_string(),
            );
        }
        let cancellation = match self
            .background_runs
            .cancel(&thread_id, QUIESCENCE_TIMEOUT)
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
        self.supervisor.release_runtime_thread(&thread_id);
        let (thread_status, status_label, terminal_reason) = match cancellation.outcome {
            RunCancellationTerminalOutcome::Cancelled => {
                (AgentThreadStatus::Cancelled, "cancelled", reason.clone())
            }
            RunCancellationTerminalOutcome::Interrupted => (
                AgentThreadStatus::Interrupted,
                "interrupted",
                "cancellation deadline exceeded; cleanup could not be confirmed".to_owned(),
            ),
        };
        if let Err(error) = recorder.append_finalized(&RunCancellationFinalizedEntry {
            request_id,
            run_scope_id: cancellation.run_scope_id,
            outcome: cancellation.outcome,
            cleanup_complete: cancellation.cleanup_complete,
            active_effects: cancellation.active_effects,
            active_tasks: cancellation.active_tasks,
            reason: terminal_reason.clone(),
            finalized_at_ms: unix_time_ms(),
        }) {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::Internal,
                error.to_string(),
            );
        }
        let interrupted = ControlEntry::AgentRunInterrupted(AgentRunInterruptedEntry {
            thread_id: thread_id.clone(),
            attempt_id: cancellation.thread.attempt_id,
            reason: terminal_reason.clone(),
        });
        let terminal = ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
            thread_id: thread_id.clone(),
            status: thread_status,
            reason: Some(terminal_reason.clone()),
            updated_at_ms: Some(unix_time_ms()),
        });
        for control in [terminal, interrupted] {
            if let Err(error) = session
                .append_control(control.clone())
                .and_then(|()| handler.handle(RunEvent::Control(control)))
            {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    error.to_string(),
                );
            }
        }
        ToolResult::ok(
            call.id.clone(),
            call.name.clone(),
            serde_json::to_string(&json!({
                "thread_id": thread_id.as_str(),
                "previous_status": thread_status_label(previous_status),
                "status": status_label,
                "reason": terminal_reason,
                "cleanup_complete": cancellation.cleanup_complete,
                "next_action": "do not wait for this agent; report the durable terminal status"
            }))
            .unwrap_or_else(|error| format!("failed to serialize agent cancel result: {error}")),
            ToolResultMeta {
                details: json!({
                    "thread_id": thread_id.as_str(),
                    "previous_status": thread_status_label(previous_status),
                    "status": status_label,
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

fn full_agent_result_delivery(
    session: &Session,
    thread_id: &AgentThreadId,
    output_hash: &str,
) -> Option<AgentThreadResultDeliveredEntry> {
    let mut delivered_chars = 0usize;
    let mut full_delivery = None;
    for entry in session.entries() {
        let SessionLogEntry::Control(ControlEntry::AgentThreadResultDelivered(delivered)) = entry
        else {
            continue;
        };
        if delivered.thread_id != *thread_id || delivered.output_hash != output_hash {
            continue;
        }
        let page_end = delivered
            .offset_chars
            .saturating_add(delivered.returned_chars);
        if delivered.offset_chars <= delivered_chars {
            delivered_chars = delivered_chars.max(page_end);
        }
        if !delivered.truncated
            && delivered.total_chars > 0
            && delivered_chars >= delivered.total_chars
        {
            full_delivery = Some(delivered.clone());
        }
    }
    full_delivery
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
