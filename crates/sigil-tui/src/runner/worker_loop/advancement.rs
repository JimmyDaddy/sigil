use super::*;
use crate::runner::V2CompactionPreviewState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runner) enum WorkerAdvancementControl {
    PollCommand,
    SkipCommandPoll,
}

pub(in crate::runner) struct WorkerAdvancementContext<'a, P> {
    pub(in crate::runner) runtime: &'a tokio::runtime::Runtime,
    pub(in crate::runner) agent: &'a mut Arc<Agent<P>>,
    pub(in crate::runner) root_config: &'a RootConfig,
    pub(in crate::runner) provider_capabilities: &'a ProviderCapabilities,
    pub(in crate::runner) workspace_root: &'a PathBuf,
    pub(in crate::runner) options: &'a AgentRunOptions,
    pub(in crate::runner) message_tx: &'a mpsc::Sender<WorkerMessage>,
    pub(in crate::runner) mcp_event_rx: &'a mpsc::Receiver<McpRuntimeEvent>,
    pub(in crate::runner) elicitation_handler: &'a Arc<ChannelMcpElicitationHandler>,
    pub(in crate::runner) mcp_event_handler: &'a Arc<ChannelMcpRuntimeEventHandler>,
    pub(in crate::runner) context_resolver: &'a sigil_runtime::RequestContextResolver,
    pub(in crate::runner) state: &'a mut WorkerLoopState,
}

impl<'a, P> WorkerAdvancementContext<'a, P> {
    fn reborrow(&mut self) -> WorkerAdvancementContext<'_, P> {
        WorkerAdvancementContext {
            runtime: self.runtime,
            agent: &mut *self.agent,
            root_config: self.root_config,
            provider_capabilities: self.provider_capabilities,
            workspace_root: self.workspace_root,
            options: self.options,
            message_tx: self.message_tx,
            mcp_event_rx: self.mcp_event_rx,
            elicitation_handler: self.elicitation_handler,
            mcp_event_handler: self.mcp_event_handler,
            context_resolver: self.context_resolver,
            state: &mut *self.state,
        }
    }
}

pub(in crate::runner) fn advance_worker_loop<P>(
    mut context: WorkerAdvancementContext<'_, P>,
) -> WorkerAdvancementControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    if matches!(
        advance_refreshes(context.reborrow()),
        WorkerAdvancementControl::SkipCommandPoll
    ) || matches!(
        advance_compaction_results(context.reborrow()),
        WorkerAdvancementControl::SkipCommandPoll
    ) || matches!(
        advance_run_results(context.reborrow()),
        WorkerAdvancementControl::SkipCommandPoll
    ) || matches!(
        advance_idle_compaction(context.reborrow()),
        WorkerAdvancementControl::SkipCommandPoll
    ) || matches!(
        advance_background_agents(context.reborrow()),
        WorkerAdvancementControl::SkipCommandPoll
    ) || matches!(
        advance_pending_agent_continuations(context.reborrow()),
        WorkerAdvancementControl::SkipCommandPoll
    ) || matches!(
        advance_conversation_queue(context.reborrow()),
        WorkerAdvancementControl::SkipCommandPoll
    ) {
        WorkerAdvancementControl::SkipCommandPoll
    } else {
        WorkerAdvancementControl::PollCommand
    }
}

fn advance_refreshes<P>(context: WorkerAdvancementContext<'_, P>) -> WorkerAdvancementControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerAdvancementContext {
        runtime,
        agent,
        root_config,
        provider_capabilities,
        options,
        message_tx,
        mcp_event_rx,
        elicitation_handler,
        mcp_event_handler,
        state,
        ..
    } = context;
    while let Ok(event) = mcp_event_rx.try_recv() {
        match event {
            McpRuntimeEvent::Progress(notification) => {
                let _ = message_tx.send(WorkerMessage::McpProgress { notification });
            }
            McpRuntimeEvent::ListChanged(notification) => {
                state
                    .refresh
                    .pending_mcp_servers
                    .insert(notification.server_name.clone());
                let _ = message_tx.send(WorkerMessage::McpListChanged { notification });
            }
        }
    }

    if state.run.active.is_none()
        && !state.refresh.pending_mcp_servers.is_empty()
        && Instant::now() >= state.refresh.next_mcp_retry_at
    {
        let shared_registry_blocked = refresh_pending_mcp_servers(
            runtime,
            agent,
            root_config,
            provider_capabilities,
            options,
            message_tx,
            Arc::clone(elicitation_handler),
            Arc::clone(mcp_event_handler),
            state
                .session
                .current
                .as_ref()
                .and_then(Session::mutation_event_recorder),
            state
                .session
                .current
                .as_ref()
                .and_then(|session| session.egress_audit_recorder().ok()),
            &mut state.refresh.pending_mcp_servers,
        );
        state.refresh.next_mcp_retry_at = if shared_registry_blocked {
            Instant::now() + MCP_REFRESH_RETRY_INTERVAL
        } else {
            Instant::now()
        };
    }

    drain_provider_status_results(
        &state.refresh.provider_status_rx,
        &mut state.refresh.provider_status_tasks,
        message_tx,
    );
    WorkerAdvancementControl::PollCommand
}

fn advance_compaction_results<P>(
    context: WorkerAdvancementContext<'_, P>,
) -> WorkerAdvancementControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerAdvancementContext {
        runtime,
        agent,
        root_config,
        workspace_root: _,
        options,
        message_tx,
        elicitation_handler,
        context_resolver: _,
        state,
        ..
    } = context;
    while let Ok(preparation_result) = state.compaction.preparation_rx.try_recv() {
        match preparation_result {
            CompactionPreparationTaskResult::Manual {
                request_id,
                session_scope_id,
                result,
            } => {
                if !state
                    .compaction
                    .preparation_tasks
                    .accept_result(request_id, &session_scope_id)
                {
                    continue;
                }
                let Some(session) = state.session.current.as_ref() else {
                    continue;
                };
                if state.run.active.is_some() || session.session_scope_id() != session_scope_id {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "discarded stale V2 compaction preparation".to_owned(),
                    ));
                    continue;
                }
                match result {
                    Ok(prepared) => {
                        let effective_config = effective_compaction_config(
                            session.provider_name(),
                            session.model_name(),
                            &options.compaction_config,
                        );
                        let current_preview = session
                            .v2_compaction_preview(effective_config.tail_messages)
                            .ok()
                            .flatten();
                        if current_preview.as_ref() != Some(&prepared.review.preview) {
                            let _ = message_tx.send(WorkerMessage::Notice(
                                    "discarded stale V2 compaction preparation after session history changed"
                                        .to_owned(),
                                ));
                            continue;
                        }
                        state.compaction.pending = prepared.pending;
                        let _ = message_tx.send(WorkerMessage::V2CompactionPreviewed {
                            state: V2CompactionPreviewState::Review(Box::new(prepared.review)),
                        });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                            "V2 compaction review failed: {error}"
                        )));
                    }
                }
            }
            CompactionPreparationTaskResult::Idle {
                request_id,
                session_scope_id,
                result,
            } => {
                if !state
                    .compaction
                    .preparation_tasks
                    .accept_result(request_id, &session_scope_id)
                {
                    continue;
                }
                let idle_frontier_is_current =
                    state.session.current.as_ref().is_some_and(|session| {
                        session.session_scope_id() == session_scope_id
                            && session
                                .conversation_queue_projection()
                                .items
                                .iter()
                                .all(|item| item.status.is_terminal())
                    }) && state.run.active.is_none()
                        && state.session.pending_agent_result_continuations.is_empty()
                        && state.compaction.pending.is_none();
                if !idle_frontier_is_current {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "discarded stale automatic compaction preparation".to_owned(),
                    ));
                    continue;
                }
                match result {
                    Ok(prepared) => {
                        state.compaction.idle_auto = prepared.state;
                        finish_idle_auto_compaction(
                            prepared.preparation,
                            &mut state.session.current,
                            &state.session.log_path,
                            message_tx,
                        );
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "automatic compaction preflight was not applied: {error}"
                        )));
                    }
                }
            }
            CompactionPreparationTaskResult::PreTurn {
                request_id,
                session_scope_id,
                result,
            } => {
                if !state
                    .compaction
                    .preparation_tasks
                    .accept_result(request_id, &session_scope_id)
                {
                    continue;
                }
                let Some(session) = state.session.current.as_ref() else {
                    continue;
                };
                if state.run.active.is_some() || session.session_scope_id() != session_scope_id {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "discarded stale queued pre-turn preparation".to_owned(),
                    ));
                    continue;
                }
                match result {
                    Ok(prepared)
                        if session
                            .conversation_queue_projection()
                            .next_dispatchable
                            .as_ref()
                            == Some(&prepared.queue_id) =>
                    {
                        state.session.pending_queued_pre_turn_preparation = Some(*prepared);
                    }
                    Ok(_) => {
                        let _ = message_tx.send(WorkerMessage::Notice(
                            "discarded queued pre-turn preparation after queue frontier changed"
                                .to_owned(),
                        ));
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                                "queued pre-turn admission was not evaluated; queued input was not sent: {error}"
                            )));
                    }
                }
            }
            CompactionPreparationTaskResult::Overflow {
                request_id,
                session_scope_id,
                result,
            } => {
                if !state
                    .compaction
                    .preparation_tasks
                    .accept_result(request_id, &session_scope_id)
                {
                    continue;
                }
                let prepared = match result {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                            "overflow recovery preparation task failed: {error}"
                        )));
                        continue;
                    }
                };
                let source_is_current = state
                    .session
                    .current
                    .as_ref()
                    .filter(|session| {
                        state.run.active.is_none() && session.session_scope_id() == session_scope_id
                    })
                    .and_then(|session| {
                        exact_context_window_rejection_source(
                            session,
                            &prepared.source_logical_run_id,
                        )
                        .ok()
                        .flatten()
                    })
                    .is_some_and(|source| source == prepared.source_physical_attempt_id);
                if !source_is_current {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "discarded stale overflow recovery preparation".to_owned(),
                    ));
                    let _ = message_tx.send(WorkerMessage::RunFailed(prepared.original_run_error));
                    continue;
                }
                let pending = match prepared.preparation {
                    Ok(pending) => pending,
                    Err(preparation_error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "overflow recovery is unavailable: {preparation_error}"
                        )));
                        let _ =
                            message_tx.send(WorkerMessage::RunFailed(prepared.original_run_error));
                        continue;
                    }
                };
                let compaction_request_id = pending.request_id();
                let folded_event_count = pending.folded_event_count();
                let frozen_request = pending.frozen_target_request();
                let applied = state
                    .session
                    .current
                    .as_ref()
                    .map(|session| pending.apply(session, &state.session.log_path));
                let outcome = match applied {
                    Some(Ok(outcome)) => outcome,
                    Some(Err(apply_error)) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "overflow recovery compaction was not applied: {apply_error:#}"
                        )));
                        let _ =
                            message_tx.send(WorkerMessage::RunFailed(prepared.original_run_error));
                        continue;
                    }
                    None => {
                        let _ =
                            message_tx.send(WorkerMessage::RunFailed(prepared.original_run_error));
                        continue;
                    }
                };
                let Some(session) = state.session.current.as_ref() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "overflow recovery applied without a loaded session".to_owned(),
                    ));
                    continue;
                };
                match load_session_with_runtime_attachments(
                    session.provider_name(),
                    session.model_name(),
                    &state.session.log_path,
                    Some(session),
                ) {
                    Ok(reloaded) => {
                        let entries = reloaded.entries().to_vec();
                        state.session.current = Some(reloaded);
                        let _ = message_tx.send(WorkerMessage::V2CompactionApplied {
                            request_id: compaction_request_id,
                            source: V2CompactionApplySource::OverflowRecovery,
                            compaction_id: outcome.compaction_id,
                            folded_event_count,
                            entries,
                        });
                        match start_portable_overflow_recovery_run(
                            runtime,
                            Arc::clone(agent),
                            &state.agent.supervisor,
                            root_config,
                            agent.tool_registry(),
                            options,
                            &state.agent.background_runs,
                            &mut state.session.current,
                            &state.run.result_tx,
                            message_tx,
                            Arc::clone(elicitation_handler),
                            &mut state.run.next_id,
                            frozen_request,
                            format!("overflow-recovery-{}", prepared.source_physical_attempt_id),
                        ) {
                            Ok(recovery_run) => state.run.active = Some(recovery_run),
                            Err(start_error) => {
                                let _ = message_tx.send(WorkerMessage::Notice(format!(
                                        "overflow recovery was applied but its one-shot retry could not start: {start_error:#}"
                                    )));
                                let _ = message_tx
                                    .send(WorkerMessage::RunFailed(prepared.original_run_error));
                            }
                        }
                    }
                    Err(reload_error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "failed to reload applied overflow recovery: {reload_error:#}"
                        )));
                        let _ =
                            message_tx.send(WorkerMessage::RunFailed(prepared.original_run_error));
                    }
                }
            }
        }
    }
    WorkerAdvancementControl::PollCommand
}

fn advance_run_results<P>(context: WorkerAdvancementContext<'_, P>) -> WorkerAdvancementControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerAdvancementContext {
        runtime,
        agent,
        root_config,
        workspace_root,
        options,
        message_tx,
        elicitation_handler,
        context_resolver,
        state,
        ..
    } = context;
    while let Ok(mut task_result) = state.run.result_rx.try_recv() {
        if state.run.discarded_ids.remove(&task_result.run_id) {
            continue;
        }
        elicitation_handler.set_audit_buffer(None);
        state.run.active = None;
        // The completed run returns the authoritative in-memory session for its own appends. Queue
        // controls accepted while that session was detached were already persisted through the
        // same linear writer, so merge only that tracked delta instead of rereading the JSONL.
        task_result
            .session
            .record_durably_appended_controls(state.session.detached_durable_controls.drain(..));
        state.session.current = Some(task_result.session);
        match task_result.payload {
            RunTaskPayload::Chat {
                result: Ok(run_result),
                plan_mode,
                queue_id,
                agent_result_continuation_thread_ids,
                ..
            } => {
                if let Some(queue_id) = queue_id.as_ref() {
                    append_queue_status_and_notify(
                        &mut state.session.current,
                        message_tx,
                        queue_id.clone(),
                        ConversationInputStatus::Delivered,
                        None,
                    );
                }
                if !agent_result_continuation_thread_ids.is_empty() {
                    append_agent_result_continuation_status_and_notify(
                        &mut state.session.current,
                        message_tx,
                        &agent_result_continuation_thread_ids,
                        AgentResultContinuationStatus::Completed,
                        Some("parent continuation completed"),
                    );
                }
                if plan_mode
                    && let Err(error) = append_plan_draft(
                        root_config,
                        workspace_root,
                        &state.session.log_path,
                        &mut state.session.current,
                        &run_result.final_text,
                        run_result.final_message_id.clone(),
                        task_result.run_id,
                    )
                {
                    let _ = message_tx.send(WorkerMessage::Notice(error));
                }
                let entries = state
                    .session
                    .current
                    .as_ref()
                    .map(|session| session.entries().to_vec())
                    .unwrap_or_default();
                let message = if plan_mode {
                    WorkerMessage::PlanRunFinished {
                        result: run_result,
                        entries,
                    }
                } else {
                    WorkerMessage::RunFinished {
                        result: run_result,
                        entries,
                    }
                };
                let _ = message_tx.send(message);
                state
                    .compaction
                    .idle_auto
                    .request_after_successful_chat_run();
            }
            RunTaskPayload::Agent {
                profile_id,
                result: Ok(run_result),
            } => {
                let entries = state
                    .session
                    .current
                    .as_ref()
                    .map(|session| session.entries().to_vec())
                    .unwrap_or_default();
                let _ = message_tx.send(WorkerMessage::AgentRunFinished {
                    profile_id,
                    result: run_result,
                    entries,
                });
            }
            RunTaskPayload::Chat {
                result: Err(error),
                plan_mode,
                queue_id,
                provider_logical_run_id,
                agent_result_continuation_thread_ids,
            } => {
                if let Some(queue_id) = queue_id.as_ref() {
                    let classification = state.session.current
                            .as_ref()
                            .ok_or_else(|| {
                                "conversation queue recovery requires a loaded session".to_owned()
                            })
                            .and_then(|session| {
                                let attempts = session
                                    .provider_physical_attempt_projection()
                                    .map_err(|attempt_error| {
                                        format!(
                                            "provider attempt evidence is unavailable: {attempt_error:#}"
                                        )
                                    })?;
                                classify_promoted_queued_conversation(session, &attempts, queue_id)
                            });
                    match classification {
                        Ok(QueuedConversationTerminalClassification::Delivered { reason }) => {
                            append_queue_status_and_notify(
                                    &mut state.session.current,
                                    message_tx,
                                    queue_id.clone(),
                                    ConversationInputStatus::Delivered,
                                    reason.or_else(|| {
                                        Some(
                                            "queued provider attempt reached a terminal after output or side effects"
                                                .to_owned(),
                                        )
                                    }),
                                );
                        }
                        Ok(QueuedConversationTerminalClassification::Rejected { reason }) => {
                            append_queue_failure_and_pause_and_notify(
                                &state.session.log_path,
                                &mut state.session.current,
                                &mut state.session.detached_durable_controls,
                                message_tx,
                                queue_id.clone(),
                                format!("{reason}: {error}"),
                            );
                        }
                        Ok(QueuedConversationTerminalClassification::Stale { reason })
                        | Err(reason) => {
                            append_queue_status_and_notify(
                                &mut state.session.current,
                                message_tx,
                                queue_id.clone(),
                                ConversationInputStatus::Stale,
                                Some(format!("{reason}: {error}")),
                            );
                        }
                    }
                }
                if !agent_result_continuation_thread_ids.is_empty() {
                    append_agent_result_continuation_status_and_notify(
                        &mut state.session.current,
                        message_tx,
                        &agent_result_continuation_thread_ids,
                        AgentResultContinuationStatus::Failed,
                        Some(error.as_str()),
                    );
                }

                let mut overflow_preparation_started = false;
                if queue_id.is_none()
                    && !plan_mode
                    && agent_result_continuation_thread_ids.is_empty()
                    && let Some(logical_run_id) = provider_logical_run_id.as_deref()
                {
                    let source_physical_attempt_id = match state.session.current.as_ref() {
                        Some(session) => {
                            match exact_context_window_rejection_source(session, logical_run_id) {
                                Ok(source_physical_attempt_id) => source_physical_attempt_id,
                                Err(source_error) => {
                                    let _ = message_tx.send(WorkerMessage::Notice(format!(
                                        "overflow recovery evidence is unavailable: {source_error:#}"
                                    )));
                                    None
                                }
                            }
                        }
                        None => None,
                    };
                    if let Some(source_physical_attempt_id) = source_physical_attempt_id {
                        let Some(session) = state.session.current.as_ref() else {
                            let _ = message_tx.send(WorkerMessage::Notice(
                                "overflow recovery requires a loaded session".to_owned(),
                            ));
                            let _ = message_tx.send(WorkerMessage::RunFailed(error));
                            continue;
                        };
                        let request_id = state.compaction.next_request_id;
                        state.compaction.next_request_id =
                            state.compaction.next_request_id.saturating_add(1);
                        let expected_session_scope_id = session.session_scope_id().to_owned();
                        let provider_name = session.provider_name().to_owned();
                        let model_name = session.model_name().to_owned();
                        let root_config = root_config.clone();
                        let workspace_root = workspace_root.clone();
                        let session_log_path = state.session.log_path.clone();
                        let options = options.clone();
                        let tools = agent.tool_registry().specs();
                        let runtime_handle = runtime.handle().clone();
                        let overflow_context_resolver = context_resolver.clone();
                        let runtime_attachments =
                            CapturedSessionRuntimeAttachments::from_session(Some(session));
                        let preparation_agent = Arc::clone(agent);
                        let source_logical_run_id = logical_run_id.to_owned();
                        let original_run_error = error.clone();
                        state.compaction.preparation_tasks.start_overflow(
                                runtime,
                                request_id,
                                expected_session_scope_id.clone(),
                                state.compaction.preparation_tx.clone(),
                                move || {
                                    let preparation = (|| {
                                        let session =
                                            load_session_with_captured_runtime_attachments(
                                                &provider_name,
                                                &model_name,
                                                &session_log_path,
                                                &runtime_attachments,
                                            )
                                            .map_err(|error| format!("{error:#}"))?;
                                        if session.session_scope_id() != expected_session_scope_id {
                                            return Err(
                                                "overflow recovery preparation loaded a different session scope"
                                                    .to_owned(),
                                            );
                                        }
                                        runtime_handle
                                            .block_on(prepare_overflow_recovery_compaction(
                                                request_id,
                                                &root_config,
                                                &workspace_root,
                                                &session_log_path,
                                                &session,
                                                &options,
                                                tools,
                                                source_physical_attempt_id.clone(),
                                                preparation_agent.provider(),
                                                &overflow_context_resolver,
                                            ))
                                            .map_err(|error| format!("{error:#}"))
                                    })();
                                    Ok(OverflowV2CompactionPreparation {
                                        source_physical_attempt_id,
                                        source_logical_run_id,
                                        original_run_error,
                                        preparation,
                                    })
                                },
                            );
                        let _ = message_tx.send(WorkerMessage::Notice(
                                "context window was rejected before generation; preparing one owned overflow recovery"
                                    .to_owned(),
                            ));
                        overflow_preparation_started = true;
                    }
                }
                if !overflow_preparation_started {
                    let _ = message_tx.send(WorkerMessage::RunFailed(error));
                }
            }
            RunTaskPayload::Agent {
                result: Err(error), ..
            } => {
                let _ = message_tx.send(WorkerMessage::RunFailed(error));
            }
            RunTaskPayload::Task {
                task_id,
                result: Ok(status),
            } => {
                let entries = state
                    .session
                    .current
                    .as_ref()
                    .map(|session| session.entries().to_vec())
                    .unwrap_or_default();
                let _ = message_tx.send(WorkerMessage::TaskRunFinished {
                    task_id,
                    status,
                    entries,
                });
            }
            RunTaskPayload::Task {
                task_id: _,
                result: Err(error),
            } => {
                let _ = message_tx.send(WorkerMessage::RunFailed(error));
            }
        }
    }
    WorkerAdvancementControl::PollCommand
}

fn advance_idle_compaction<P>(context: WorkerAdvancementContext<'_, P>) -> WorkerAdvancementControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerAdvancementContext {
        runtime,
        agent,
        root_config,
        workspace_root,
        options,
        context_resolver,
        state,
        ..
    } = context;
    let conversation_queue_is_idle = state.session.current.as_ref().is_some_and(|session| {
        session
            .conversation_queue_projection()
            .items
            .iter()
            .all(|item| item.status.is_terminal())
    });
    if state.run.active.is_none()
        && conversation_queue_is_idle
        && state.session.pending_agent_result_continuations.is_empty()
        && state.compaction.pending.is_none()
        && !state.compaction.preparation_tasks.has_active()
        && state.compaction.idle_auto.is_requested()
        && let Some(session) = state.session.current.as_ref()
    {
        let request_id = state.compaction.next_request_id;
        state.compaction.next_request_id = state.compaction.next_request_id.saturating_add(1);
        let expected_session_scope_id = session.session_scope_id().to_owned();
        let provider_name = session.provider_name().to_owned();
        let model_name = session.model_name().to_owned();
        let root_config = root_config.clone();
        let workspace_root = workspace_root.clone();
        let session_log_path = state.session.log_path.clone();
        let options = options.clone();
        let tools = agent.tool_registry().specs();
        let runtime_handle = runtime.handle().clone();
        let idle_context_resolver = context_resolver.clone();
        let runtime_attachments = CapturedSessionRuntimeAttachments::from_session(Some(session));
        let mut idle_auto_state = state.compaction.idle_auto.clone();
        state.compaction.idle_auto.cancel_requested_run();
        state.compaction.preparation_tasks.start_idle(
            runtime,
            request_id,
            expected_session_scope_id.clone(),
            state.compaction.preparation_tx.clone(),
            move || {
                let session = load_session_with_captured_runtime_attachments(
                    &provider_name,
                    &model_name,
                    &session_log_path,
                    &runtime_attachments,
                )
                .map_err(|error| format!("{error:#}"))?;
                if session.session_scope_id() != expected_session_scope_id {
                    return Err(
                        "automatic compaction preparation loaded a different session scope"
                            .to_owned(),
                    );
                }
                let preparation = prepare_idle_auto_compaction(
                    &mut idle_auto_state,
                    &root_config,
                    &workspace_root,
                    &session_log_path,
                    &session,
                    &options,
                    tools,
                    &idle_context_resolver,
                    &runtime_handle,
                )
                .map_err(|error| format!("{error:#}"))?;
                Ok(IdleV2CompactionPreparation {
                    state: idle_auto_state,
                    preparation,
                })
            },
        );
    }
    WorkerAdvancementControl::PollCommand
}

fn advance_background_agents<P>(
    context: WorkerAdvancementContext<'_, P>,
) -> WorkerAdvancementControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerAdvancementContext {
        runtime,
        agent,
        root_config,
        options,
        message_tx,
        elicitation_handler,
        state,
        ..
    } = context;
    if state.run.active.is_none() {
        if Instant::now() >= state.refresh.next_terminal_task_refresh_at {
            state.refresh.next_terminal_task_refresh_at =
                Instant::now() + TERMINAL_TASK_REFRESH_INTERVAL;
            match refresh_terminal_task_statuses(
                runtime,
                agent.tool_registry(),
                options,
                &state.session.log_path,
                &mut state.session.current,
            ) {
                Ok(updates) => {
                    for (entry, entries) in updates {
                        let _ =
                            message_tx.send(WorkerMessage::TerminalTaskUpdated { entry, entries });
                    }
                }
                Err(error) => {
                    let _ = message_tx.send(WorkerMessage::Notice(error));
                }
            }
        }

        let completed_agent_threads = collect_finished_background_agent_runs(
            runtime,
            &state.agent.background_runs,
            &state.agent.supervisor,
            root_config,
            agent.tool_registry(),
            &mut state.session.current,
            message_tx,
        );
        if !completed_agent_threads.is_empty() {
            let new_continuation_threads = agent_result_continuation_new_thread_ids(
                state.session.current.as_ref(),
                &completed_agent_threads,
            );
            if !new_continuation_threads.is_empty()
                && let Err(error) = append_agent_result_continuation_status_entries(
                    &state.session.log_path,
                    &mut state.session.current,
                    &new_continuation_threads,
                    AgentResultContinuationStatus::Pending,
                    Some("child agent result ready"),
                )
            {
                let _ = message_tx.send(WorkerMessage::RunFailed(error));
                return WorkerAdvancementControl::SkipCommandPoll;
            }
            let (blocking, non_blocking) = partition_agent_result_continuations(
                state.session.current.as_ref(),
                completed_agent_threads,
            );
            extend_agent_thread_ids_unique(
                &mut state.session.pending_agent_result_continuations,
                non_blocking,
            );
            let queued_input_ready = state
                .session
                .current
                .as_ref()
                .and_then(|session| session.conversation_queue_projection().next_dispatchable)
                .is_some();
            let mut continuation_threads = blocking;
            if !queued_input_ready {
                continuation_threads.append(&mut state.session.pending_agent_result_continuations);
            }
            if continuation_threads.is_empty() {
                return WorkerAdvancementControl::SkipCommandPoll;
            }
            state.run.active = start_agent_result_continuation_run(
                runtime,
                Arc::clone(agent),
                &state.agent.supervisor,
                root_config,
                &state.session.log_path,
                agent.tool_registry(),
                options,
                &state.agent.background_runs,
                &mut state.session.current,
                &state.run.result_tx,
                message_tx,
                Arc::clone(elicitation_handler),
                &mut state.run.next_id,
                continuation_threads,
            );
            if state.run.active.is_some() {
                return WorkerAdvancementControl::SkipCommandPoll;
            }
        }
    }

    WorkerAdvancementControl::PollCommand
}

fn advance_pending_agent_continuations<P>(
    context: WorkerAdvancementContext<'_, P>,
) -> WorkerAdvancementControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerAdvancementContext {
        runtime,
        agent,
        root_config,
        options,
        message_tx,
        elicitation_handler,
        state,
        ..
    } = context;
    if state.run.active.is_none() {
        let queued_input_ready = state
            .session
            .current
            .as_ref()
            .and_then(|session| session.conversation_queue_projection().next_dispatchable)
            .is_some();
        if !queued_input_ready && !state.session.pending_agent_result_continuations.is_empty() {
            let continuation_threads =
                std::mem::take(&mut state.session.pending_agent_result_continuations);
            state.run.active = start_agent_result_continuation_run(
                runtime,
                Arc::clone(agent),
                &state.agent.supervisor,
                root_config,
                &state.session.log_path,
                agent.tool_registry(),
                options,
                &state.agent.background_runs,
                &mut state.session.current,
                &state.run.result_tx,
                message_tx,
                Arc::clone(elicitation_handler),
                &mut state.run.next_id,
                continuation_threads,
            );
            if state.run.active.is_some() {
                return WorkerAdvancementControl::SkipCommandPoll;
            }
        }
    }

    WorkerAdvancementControl::PollCommand
}

fn advance_conversation_queue<P>(
    context: WorkerAdvancementContext<'_, P>,
) -> WorkerAdvancementControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerAdvancementContext {
        runtime,
        agent,
        root_config,
        workspace_root,
        options,
        message_tx,
        elicitation_handler,
        context_resolver,
        state,
        ..
    } = context;
    if state.run.active.is_none() {
        let next_queue_id = state
            .session
            .current
            .as_ref()
            .and_then(|session| session.conversation_queue_projection().next_dispatchable);
        if state.session.pending_queued_pre_turn_preparation.is_none()
            && !state.compaction.preparation_tasks.has_active()
            && let Some(queue_id) = next_queue_id.clone()
            && let Some(session) = state.session.current.as_ref()
        {
            let request_id = state.compaction.next_request_id;
            state.compaction.next_request_id = state.compaction.next_request_id.saturating_add(1);
            let expected_session_scope_id = session.session_scope_id().to_owned();
            let provider_name = session.provider_name().to_owned();
            let model_name = session.model_name().to_owned();
            let root_config = root_config.clone();
            let workspace_root = workspace_root.clone();
            let session_log_path = state.session.log_path.clone();
            let exact_prompts = state.session.exact_prompts.clone();
            let options = options.clone();
            let tools = agent.tool_registry().specs();
            let runtime_handle = runtime.handle().clone();
            let queue_context_resolver = context_resolver.clone();
            let runtime_attachments =
                CapturedSessionRuntimeAttachments::from_session(Some(session));
            state.compaction.preparation_tasks.start_pre_turn(
                runtime,
                request_id,
                expected_session_scope_id.clone(),
                state.compaction.preparation_tx.clone(),
                move || {
                    let session = load_session_with_captured_runtime_attachments(
                        &provider_name,
                        &model_name,
                        &session_log_path,
                        &runtime_attachments,
                    )
                    .map_err(|error| format!("{error:#}"))?;
                    if session.session_scope_id() != expected_session_scope_id {
                        return Err(
                            "queued pre-turn preparation loaded a different session scope"
                                .to_owned(),
                        );
                    }
                    if session
                        .conversation_queue_projection()
                        .next_dispatchable
                        .as_ref()
                        != Some(&queue_id)
                    {
                        return Err(
                            "queued pre-turn preparation loaded a different queue frontier"
                                .to_owned(),
                        );
                    }
                    let admission = prepare_next_queued_conversation_pre_turn_admission(
                        &root_config,
                        &workspace_root,
                        &session_log_path,
                        &session,
                        &exact_prompts,
                        &options.memory_config,
                        tools,
                        options.reasoning_effort.clone(),
                        options.traffic_partition_key.clone(),
                        &queue_context_resolver,
                        &runtime_handle,
                    )
                    .map_err(|error| format!("{error:#}"))?;
                    Ok(PreTurnV2CompactionPreparation {
                        queue_id,
                        admission,
                    })
                },
            );
        }

        let candidate = match state.session.pending_queued_pre_turn_preparation.take() {
            None => {
                if next_queue_id.is_none() {
                    state.session.last_queued_pre_turn_block = None;
                }
                None
            }
            Some(PreTurnV2CompactionPreparation {
                admission: QueuedConversationPreTurnAdmission::NoQueuedInput,
                ..
            }) => {
                state.session.last_queued_pre_turn_block = None;
                None
            }
            Some(PreTurnV2CompactionPreparation {
                admission:
                    QueuedConversationPreTurnAdmission::Blocked {
                        queue_id,
                        reason,
                        candidate,
                    },
                ..
            }) => match candidate {
                Some(candidate) => {
                    let notice = format!(
                        "queued pre-turn compaction is unavailable ({reason}); dispatching the unchanged frozen request"
                    );
                    let block = (queue_id, notice.clone());
                    if state.session.last_queued_pre_turn_block.as_ref() != Some(&block) {
                        let _ = message_tx.send(WorkerMessage::Notice(notice));
                    }
                    state.session.last_queued_pre_turn_block = Some(block);
                    Some(*candidate)
                }
                None => {
                    let block = (queue_id, reason);
                    if state.session.last_queued_pre_turn_block.as_ref() != Some(&block) {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "queued follow-up is waiting for a local pre-turn admission: {}",
                            block.1
                        )));
                    }
                    state.session.last_queued_pre_turn_block = Some(block);
                    None
                }
            },
            Some(PreTurnV2CompactionPreparation {
                admission: QueuedConversationPreTurnAdmission::ExactFit(admitted),
                ..
            }) => {
                state.session.last_queued_pre_turn_block = None;
                Some(admitted.candidate)
            }
            Some(PreTurnV2CompactionPreparation {
                admission: QueuedConversationPreTurnAdmission::PortablePreflightReady(pending),
                ..
            }) => {
                let Some(session) = state.session.current.as_ref() else {
                    return WorkerAdvancementControl::SkipCommandPoll;
                };
                let folded_event_count = pending.folded_event_count();
                match pending.apply_compaction(session, &state.session.log_path) {
                    Ok((candidate, outcome)) => {
                        match load_session_with_runtime_attachments(
                            session.provider_name(),
                            session.model_name(),
                            &state.session.log_path,
                            state.session.current.as_ref(),
                        ) {
                            Ok(reloaded) => {
                                let entries = reloaded.entries().to_vec();
                                state.session.current = Some(reloaded);
                                state.session.last_queued_pre_turn_block = None;
                                let _ = message_tx.send(WorkerMessage::V2CompactionApplied {
                                    request_id: 0,
                                    source: V2CompactionApplySource::PreTurnPressure,
                                    compaction_id: outcome.compaction_id,
                                    folded_event_count,
                                    entries,
                                });
                                Some(candidate)
                            }
                            Err(error) => {
                                let _ = message_tx.send(WorkerMessage::Notice(format!(
                                        "queued pre-turn compaction completed but session reload failed; queued input was not sent: {error:#}"
                                    )));
                                None
                            }
                        }
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                                "queued pre-turn compaction was not applied; queued input was not sent: {error:#}"
                            )));
                        None
                    }
                }
            }
        };
        if let Some(candidate) = candidate {
            let queue_id = candidate.promotion.queue_id.clone();
            let committed = match state.session.current.as_mut() {
                Some(session) => commit_prepared_queued_conversation_candidate(
                    &state.session.log_path,
                    session,
                    candidate,
                ),
                None => Err("session state is unavailable for queued promotion".to_owned()),
            };
            match committed {
                Ok(candidate) => {
                    let provider_name = state
                        .session
                        .current
                        .as_ref()
                        .map(|session| session.provider_name().to_owned());
                    let model_name = state
                        .session
                        .current
                        .as_ref()
                        .map(|session| session.model_name().to_owned());
                    match (provider_name, model_name) {
                        (Some(provider_name), Some(model_name)) => {
                            match load_session_with_runtime_attachments(
                                &provider_name,
                                &model_name,
                                &state.session.log_path,
                                state.session.current.as_ref(),
                            ) {
                                Ok(reloaded) => {
                                    state.session.current = Some(reloaded);
                                    state.session.exact_prompts.remove(&queue_id);
                                    if let Some(session) = state.session.current.as_ref() {
                                        send_conversation_queue_update(
                                            message_tx,
                                            session.entries(),
                                        );
                                    }
                                    state.run.active = start_queued_conversation_run(
                                        runtime,
                                        Arc::clone(agent),
                                        &state.agent.supervisor,
                                        root_config,
                                        agent.tool_registry(),
                                        options,
                                        &state.agent.background_runs,
                                        &mut state.session.current,
                                        &state.run.result_tx,
                                        message_tx,
                                        Arc::clone(elicitation_handler),
                                        &mut state.run.next_id,
                                        candidate,
                                    );
                                }
                                Err(error) => {
                                    let _ = message_tx.send(WorkerMessage::Notice(format!(
                                            "queued promotion committed but session reload failed; provider dispatch was refused: {error:#}"
                                        )));
                                }
                            }
                        }
                        _ => {
                            let _ = message_tx.send(WorkerMessage::Notice(
                                    "queued promotion committed but session state was unavailable; provider dispatch was refused"
                                        .to_owned(),
                                ));
                        }
                    }
                }
                Err(error) => {
                    let _ = message_tx.send(WorkerMessage::Notice(format!(
                        "queued promotion was not dispatched: {error}"
                    )));
                }
            }
        }
        if state.run.active.is_some() {
            return WorkerAdvancementControl::SkipCommandPoll;
        }
    }
    WorkerAdvancementControl::PollCommand
}
