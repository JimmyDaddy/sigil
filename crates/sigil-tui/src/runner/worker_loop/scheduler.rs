use super::agent_runtime::chat_agent_run_input_with_repo_context;
use super::*;
use crate::runner::V2CompactionPreviewState;

pub(in crate::runner) fn run_worker_loop<P>(
    runtime: tokio::runtime::Runtime,
    mut agent: Arc<Agent<P>>,
    root_config: RootConfig,
    provider_capabilities: ProviderCapabilities,
    workspace_root: PathBuf,
    session_log_path: PathBuf,
    options: AgentRunOptions,
    command_rx: mpsc::Receiver<WorkerCommand>,
    message_tx: mpsc::Sender<WorkerMessage>,
    mcp_handlers: WorkerLoopMcpHandlers,
) where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerLoopMcpHandlers {
        elicitation_handler,
        event_handler: mcp_event_handler,
        event_rx: mcp_event_rx,
        role_provider_builder,
        context_resolver,
    } = mcp_handlers;
    let mut current_session_log_path = session_log_path;
    let mut exact_conversation_prompts = ExactConversationPromptStore::new();
    let attachment_paths = sigil_runtime::resolve_sigil_paths(
        &root_config.storage,
        &root_config.session,
        &workspace_root,
    );
    let default_image_attachment_resolver: Arc<dyn ImageAttachmentResolver> = Arc::new(
        sigil_runtime::ControlledImageAttachmentCache::new(attachment_paths.attachments_root),
    );
    let mut current_session = match load_session_with_runtime_attachments(
        &root_config.agent.provider,
        &root_config.agent.model,
        &current_session_log_path,
        None,
    ) {
        Ok(mut session) => {
            if let Err(error) = session.try_attach_image_attachment_resolver(Arc::clone(
                &default_image_attachment_resolver,
            )) {
                let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                    "failed to attach image cache resolver: {error:#}"
                )));
                return;
            }
            mark_stale_dispatching_conversation_queue_items(
                &mut session,
                &exact_conversation_prompts,
                &message_tx,
            );
            Some(session)
        }
        Err(error) => {
            let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
            return;
        }
    };

    let (task_result_tx, task_result_rx) = mpsc::channel::<RunTaskResult>();
    let (provider_status_tx, provider_status_rx) = mpsc::channel::<ProviderStatusTaskResult>();
    let (compaction_preparation_tx, compaction_preparation_rx) =
        mpsc::channel::<CompactionPreparationTaskResult>();
    let mut active_run: Option<ActiveRun> = None;
    let mut processed_worker_command_ids = BTreeSet::<String>::new();
    let mut provider_status_tasks = ProviderStatusTaskManager::new();
    let mut compaction_preparation_tasks = CompactionPreparationTaskManager::new();
    let mut next_run_id = 1_u64;
    let mut next_v2_compaction_request_id = 1_u64;
    let mut pending_v2_compaction: Option<PendingV2Compaction> = None;
    let mut idle_auto_compaction = IdleAutoCompactionState::default();
    let mut discarded_run_ids = BTreeSet::new();
    let mut pending_mcp_refreshes = BTreeSet::new();
    let mut next_mcp_refresh_retry_at = Instant::now();
    let mut pending_agent_result_continuations =
        pending_agent_result_continuations_from_session(current_session.as_ref());
    let mut last_queued_pre_turn_block: Option<(ConversationInputQueueId, String)> = None;
    let mut pending_queued_pre_turn_preparation: Option<PreTurnV2CompactionPreparation> = None;
    let mut next_terminal_task_refresh_at = Instant::now();
    let session_entries = current_session
        .as_ref()
        .map(Session::entries)
        .unwrap_or(&[]);
    let agent_supervisor =
        match sigil_runtime::AgentProfileRegistry::from_root_config_with_workspace_and_entries(
            &root_config,
            &workspace_root,
            session_entries,
        ) {
            Ok(registry) => sigil_runtime::AgentSupervisor::new(
                registry,
                sigil_runtime::AgentBudgetPolicy::from_root_config(&root_config),
                provider_capabilities.clone(),
            ),
            Err(error) => {
                let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                return;
            }
        };
    let background_agent_runs =
        sigil_runtime::AgentToolBackgroundRuns::with_event_sink(Arc::new(WorkerAgentEventSink {
            sender: message_tx.clone(),
        }));
    let _ = message_tx.send(WorkerMessage::WorkerReady);

    loop {
        while let Ok(event) = mcp_event_rx.try_recv() {
            match event {
                McpRuntimeEvent::Progress(notification) => {
                    let _ = message_tx.send(WorkerMessage::McpProgress { notification });
                }
                McpRuntimeEvent::ListChanged(notification) => {
                    pending_mcp_refreshes.insert(notification.server_name.clone());
                    let _ = message_tx.send(WorkerMessage::McpListChanged { notification });
                }
            }
        }

        if active_run.is_none()
            && !pending_mcp_refreshes.is_empty()
            && Instant::now() >= next_mcp_refresh_retry_at
        {
            let shared_registry_blocked = refresh_pending_mcp_servers(
                &runtime,
                &mut agent,
                &root_config,
                &provider_capabilities,
                &options,
                &message_tx,
                Arc::clone(&elicitation_handler),
                Arc::clone(&mcp_event_handler),
                current_session
                    .as_ref()
                    .and_then(Session::mutation_event_recorder),
                &mut pending_mcp_refreshes,
            );
            next_mcp_refresh_retry_at = if shared_registry_blocked {
                Instant::now() + MCP_REFRESH_RETRY_INTERVAL
            } else {
                Instant::now()
            };
        }

        drain_provider_status_results(&provider_status_rx, &mut provider_status_tasks, &message_tx);

        while let Ok(preparation_result) = compaction_preparation_rx.try_recv() {
            match preparation_result {
                CompactionPreparationTaskResult::Manual {
                    request_id,
                    session_scope_id,
                    result,
                } => {
                    if !compaction_preparation_tasks.accept_result(request_id, &session_scope_id) {
                        continue;
                    }
                    let Some(session) = current_session.as_ref() else {
                        continue;
                    };
                    if active_run.is_some() || session.session_scope_id() != session_scope_id {
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
                            pending_v2_compaction = prepared.pending;
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
                    if !compaction_preparation_tasks.accept_result(request_id, &session_scope_id) {
                        continue;
                    }
                    let idle_frontier_is_current =
                        current_session.as_ref().is_some_and(|session| {
                            session.session_scope_id() == session_scope_id
                                && session
                                    .conversation_queue_projection()
                                    .items
                                    .iter()
                                    .all(|item| item.status.is_terminal())
                        }) && active_run.is_none()
                            && pending_agent_result_continuations.is_empty()
                            && pending_v2_compaction.is_none();
                    if !idle_frontier_is_current {
                        let _ = message_tx.send(WorkerMessage::Notice(
                            "discarded stale automatic compaction preparation".to_owned(),
                        ));
                        continue;
                    }
                    match result {
                        Ok(prepared) => {
                            idle_auto_compaction = prepared.state;
                            finish_idle_auto_compaction(
                                prepared.preparation,
                                &mut current_session,
                                &current_session_log_path,
                                &message_tx,
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
                    if !compaction_preparation_tasks.accept_result(request_id, &session_scope_id) {
                        continue;
                    }
                    let Some(session) = current_session.as_ref() else {
                        continue;
                    };
                    if active_run.is_some() || session.session_scope_id() != session_scope_id {
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
                            pending_queued_pre_turn_preparation = Some(*prepared);
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
                    if !compaction_preparation_tasks.accept_result(request_id, &session_scope_id) {
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
                    let source_is_current = current_session
                        .as_ref()
                        .filter(|session| {
                            active_run.is_none() && session.session_scope_id() == session_scope_id
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
                        let _ =
                            message_tx.send(WorkerMessage::RunFailed(prepared.original_run_error));
                        continue;
                    }
                    let pending = match prepared.preparation {
                        Ok(pending) => pending,
                        Err(preparation_error) => {
                            let _ = message_tx.send(WorkerMessage::Notice(format!(
                                "overflow recovery is unavailable: {preparation_error}"
                            )));
                            let _ = message_tx
                                .send(WorkerMessage::RunFailed(prepared.original_run_error));
                            continue;
                        }
                    };
                    let compaction_request_id = pending.request_id();
                    let folded_event_count = pending.folded_event_count();
                    let frozen_request = pending.frozen_target_request();
                    let applied = current_session
                        .as_ref()
                        .map(|session| pending.apply(session, &current_session_log_path));
                    let outcome = match applied {
                        Some(Ok(outcome)) => outcome,
                        Some(Err(apply_error)) => {
                            let _ = message_tx.send(WorkerMessage::Notice(format!(
                                "overflow recovery compaction was not applied: {apply_error:#}"
                            )));
                            let _ = message_tx
                                .send(WorkerMessage::RunFailed(prepared.original_run_error));
                            continue;
                        }
                        None => {
                            let _ = message_tx
                                .send(WorkerMessage::RunFailed(prepared.original_run_error));
                            continue;
                        }
                    };
                    let Some(session) = current_session.as_ref() else {
                        let _ = message_tx.send(WorkerMessage::RunFailed(
                            "overflow recovery applied without a loaded session".to_owned(),
                        ));
                        continue;
                    };
                    match load_session_with_runtime_attachments(
                        session.provider_name(),
                        session.model_name(),
                        &current_session_log_path,
                        Some(session),
                    ) {
                        Ok(reloaded) => {
                            let entries = reloaded.entries().to_vec();
                            current_session = Some(reloaded);
                            let _ = message_tx.send(WorkerMessage::V2CompactionApplied {
                                request_id: compaction_request_id,
                                source: V2CompactionApplySource::OverflowRecovery,
                                compaction_id: outcome.compaction_id,
                                folded_event_count,
                                entries,
                            });
                            match start_portable_overflow_recovery_run(
                                &runtime,
                                Arc::clone(&agent),
                                &agent_supervisor,
                                &root_config,
                                agent.tool_registry(),
                                &options,
                                &background_agent_runs,
                                &mut current_session,
                                &task_result_tx,
                                &message_tx,
                                Arc::clone(&elicitation_handler),
                                &mut next_run_id,
                                frozen_request,
                                format!(
                                    "overflow-recovery-{}",
                                    prepared.source_physical_attempt_id
                                ),
                            ) {
                                Ok(recovery_run) => active_run = Some(recovery_run),
                                Err(start_error) => {
                                    let _ = message_tx.send(WorkerMessage::Notice(format!(
                                        "overflow recovery was applied but its one-shot retry could not start: {start_error:#}"
                                    )));
                                    let _ = message_tx.send(WorkerMessage::RunFailed(
                                        prepared.original_run_error,
                                    ));
                                }
                            }
                        }
                        Err(reload_error) => {
                            let _ = message_tx.send(WorkerMessage::Notice(format!(
                                "failed to reload applied overflow recovery: {reload_error:#}"
                            )));
                            let _ = message_tx
                                .send(WorkerMessage::RunFailed(prepared.original_run_error));
                        }
                    }
                }
            }
        }

        while let Ok(task_result) = task_result_rx.try_recv() {
            if discarded_run_ids.remove(&task_result.run_id) {
                continue;
            }
            elicitation_handler.set_audit_buffer(None);
            active_run = None;
            current_session = match load_session_with_runtime_attachments(
                task_result.session.provider_name(),
                task_result.session.model_name(),
                &current_session_log_path,
                Some(&task_result.session),
            ) {
                Ok(session) => Some(session),
                Err(error) => {
                    let _ = message_tx.send(WorkerMessage::Notice(format!(
                        "session reload skipped after run: {error:#}"
                    )));
                    Some(task_result.session)
                }
            };
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
                            &mut current_session,
                            &message_tx,
                            queue_id.clone(),
                            ConversationInputStatus::Delivered,
                            None,
                        );
                    }
                    if !agent_result_continuation_thread_ids.is_empty() {
                        append_agent_result_continuation_status_and_notify(
                            &mut current_session,
                            &message_tx,
                            &agent_result_continuation_thread_ids,
                            AgentResultContinuationStatus::Completed,
                            Some("parent continuation completed"),
                        );
                    }
                    if plan_mode
                        && let Err(error) = append_plan_draft(
                            &root_config,
                            &workspace_root,
                            &current_session_log_path,
                            &mut current_session,
                            &run_result.final_text,
                            run_result.final_message_id.clone(),
                            task_result.run_id,
                        )
                    {
                        let _ = message_tx.send(WorkerMessage::Notice(error));
                    }
                    let entries = current_session
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
                    idle_auto_compaction.request_after_successful_chat_run();
                }
                RunTaskPayload::Agent {
                    profile_id,
                    result: Ok(run_result),
                } => {
                    let entries = current_session
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
                        let classification = current_session
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
                                    &mut current_session,
                                    &message_tx,
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
                                    &current_session_log_path,
                                    &mut current_session,
                                    &message_tx,
                                    queue_id.clone(),
                                    format!("{reason}: {error}"),
                                );
                            }
                            Ok(QueuedConversationTerminalClassification::Stale { reason })
                            | Err(reason) => {
                                append_queue_status_and_notify(
                                    &mut current_session,
                                    &message_tx,
                                    queue_id.clone(),
                                    ConversationInputStatus::Stale,
                                    Some(format!("{reason}: {error}")),
                                );
                            }
                        }
                    }
                    if !agent_result_continuation_thread_ids.is_empty() {
                        append_agent_result_continuation_status_and_notify(
                            &mut current_session,
                            &message_tx,
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
                        let source_physical_attempt_id = match current_session.as_ref() {
                            Some(session) => {
                                match exact_context_window_rejection_source(session, logical_run_id)
                                {
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
                            let Some(session) = current_session.as_ref() else {
                                let _ = message_tx.send(WorkerMessage::Notice(
                                    "overflow recovery requires a loaded session".to_owned(),
                                ));
                                let _ = message_tx.send(WorkerMessage::RunFailed(error));
                                continue;
                            };
                            let request_id = next_v2_compaction_request_id;
                            next_v2_compaction_request_id =
                                next_v2_compaction_request_id.saturating_add(1);
                            let expected_session_scope_id = session.session_scope_id().to_owned();
                            let provider_name = session.provider_name().to_owned();
                            let model_name = session.model_name().to_owned();
                            let root_config = root_config.clone();
                            let workspace_root = workspace_root.clone();
                            let session_log_path = current_session_log_path.clone();
                            let options = options.clone();
                            let tools = agent.tool_registry().specs();
                            let runtime_handle = runtime.handle().clone();
                            let overflow_context_resolver = context_resolver.clone();
                            let preparation_agent = Arc::clone(&agent);
                            let source_logical_run_id = logical_run_id.to_owned();
                            let original_run_error = error.clone();
                            compaction_preparation_tasks.start_overflow(
                                &runtime,
                                request_id,
                                expected_session_scope_id.clone(),
                                compaction_preparation_tx.clone(),
                                move || {
                                    let preparation = (|| {
                                        let store = JsonlSessionStore::new(&session_log_path)
                                            .map_err(|error| format!("{error:#}"))?;
                                        let session = Session::load_from_store(
                                            provider_name,
                                            model_name,
                                            store,
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
                    let entries = current_session
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

        let conversation_queue_is_idle = current_session.as_ref().is_some_and(|session| {
            session
                .conversation_queue_projection()
                .items
                .iter()
                .all(|item| item.status.is_terminal())
        });
        if active_run.is_none()
            && conversation_queue_is_idle
            && pending_agent_result_continuations.is_empty()
            && pending_v2_compaction.is_none()
            && !compaction_preparation_tasks.has_active()
            && idle_auto_compaction.is_requested()
            && let Some(session) = current_session.as_ref()
        {
            let request_id = next_v2_compaction_request_id;
            next_v2_compaction_request_id = next_v2_compaction_request_id.saturating_add(1);
            let expected_session_scope_id = session.session_scope_id().to_owned();
            let provider_name = session.provider_name().to_owned();
            let model_name = session.model_name().to_owned();
            let root_config = root_config.clone();
            let workspace_root = workspace_root.clone();
            let session_log_path = current_session_log_path.clone();
            let options = options.clone();
            let tools = agent.tool_registry().specs();
            let runtime_handle = runtime.handle().clone();
            let idle_context_resolver = context_resolver.clone();
            let mut state = idle_auto_compaction.clone();
            idle_auto_compaction.cancel_requested_run();
            compaction_preparation_tasks.start_idle(
                &runtime,
                request_id,
                expected_session_scope_id.clone(),
                compaction_preparation_tx.clone(),
                move || {
                    let store = JsonlSessionStore::new(&session_log_path)
                        .map_err(|error| format!("{error:#}"))?;
                    let session = Session::load_from_store(provider_name, model_name, store)
                        .map_err(|error| format!("{error:#}"))?;
                    if session.session_scope_id() != expected_session_scope_id {
                        return Err(
                            "automatic compaction preparation loaded a different session scope"
                                .to_owned(),
                        );
                    }
                    let preparation = prepare_idle_auto_compaction(
                        &mut state,
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
                    Ok(IdleV2CompactionPreparation { state, preparation })
                },
            );
        }

        if active_run.is_none() {
            if Instant::now() >= next_terminal_task_refresh_at {
                next_terminal_task_refresh_at = Instant::now() + TERMINAL_TASK_REFRESH_INTERVAL;
                match refresh_terminal_task_statuses(
                    &runtime,
                    agent.tool_registry(),
                    &options,
                    &current_session_log_path,
                    &mut current_session,
                ) {
                    Ok(updates) => {
                        for (entry, entries) in updates {
                            let _ = message_tx
                                .send(WorkerMessage::TerminalTaskUpdated { entry, entries });
                        }
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(error));
                    }
                }
            }

            let completed_agent_threads = collect_finished_background_agent_runs(
                &runtime,
                &background_agent_runs,
                &agent_supervisor,
                &root_config,
                agent.tool_registry(),
                &mut current_session,
                &message_tx,
            );
            if !completed_agent_threads.is_empty() {
                let new_continuation_threads = agent_result_continuation_new_thread_ids(
                    current_session.as_ref(),
                    &completed_agent_threads,
                );
                if !new_continuation_threads.is_empty()
                    && let Err(error) = append_agent_result_continuation_status_entries(
                        &current_session_log_path,
                        &mut current_session,
                        &new_continuation_threads,
                        AgentResultContinuationStatus::Pending,
                        Some("child agent result ready"),
                    )
                {
                    let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    continue;
                }
                let (blocking, non_blocking) = partition_agent_result_continuations(
                    current_session.as_ref(),
                    completed_agent_threads,
                );
                extend_agent_thread_ids_unique(
                    &mut pending_agent_result_continuations,
                    non_blocking,
                );
                let queued_input_ready = current_session
                    .as_ref()
                    .and_then(|session| session.conversation_queue_projection().next_dispatchable)
                    .is_some();
                let mut continuation_threads = blocking;
                if !queued_input_ready {
                    continuation_threads.append(&mut pending_agent_result_continuations);
                }
                if continuation_threads.is_empty() {
                    continue;
                }
                active_run = start_agent_result_continuation_run(
                    &runtime,
                    Arc::clone(&agent),
                    &agent_supervisor,
                    &root_config,
                    &current_session_log_path,
                    agent.tool_registry(),
                    &options,
                    &background_agent_runs,
                    &mut current_session,
                    &task_result_tx,
                    &message_tx,
                    Arc::clone(&elicitation_handler),
                    &mut next_run_id,
                    continuation_threads,
                );
                if active_run.is_some() {
                    continue;
                }
            }
        }

        if active_run.is_none() {
            let queued_input_ready = current_session
                .as_ref()
                .and_then(|session| session.conversation_queue_projection().next_dispatchable)
                .is_some();
            if !queued_input_ready && !pending_agent_result_continuations.is_empty() {
                let continuation_threads = std::mem::take(&mut pending_agent_result_continuations);
                active_run = start_agent_result_continuation_run(
                    &runtime,
                    Arc::clone(&agent),
                    &agent_supervisor,
                    &root_config,
                    &current_session_log_path,
                    agent.tool_registry(),
                    &options,
                    &background_agent_runs,
                    &mut current_session,
                    &task_result_tx,
                    &message_tx,
                    Arc::clone(&elicitation_handler),
                    &mut next_run_id,
                    continuation_threads,
                );
                if active_run.is_some() {
                    continue;
                }
            }
        }

        if active_run.is_none() {
            let next_queue_id = current_session
                .as_ref()
                .and_then(|session| session.conversation_queue_projection().next_dispatchable);
            if pending_queued_pre_turn_preparation.is_none()
                && !compaction_preparation_tasks.has_active()
                && let Some(queue_id) = next_queue_id.clone()
                && let Some(session) = current_session.as_ref()
            {
                let request_id = next_v2_compaction_request_id;
                next_v2_compaction_request_id = next_v2_compaction_request_id.saturating_add(1);
                let expected_session_scope_id = session.session_scope_id().to_owned();
                let provider_name = session.provider_name().to_owned();
                let model_name = session.model_name().to_owned();
                let root_config = root_config.clone();
                let workspace_root = workspace_root.clone();
                let session_log_path = current_session_log_path.clone();
                let exact_prompts = exact_conversation_prompts.clone();
                let options = options.clone();
                let tools = agent.tool_registry().specs();
                let runtime_handle = runtime.handle().clone();
                let queue_context_resolver = context_resolver.clone();
                compaction_preparation_tasks.start_pre_turn(
                    &runtime,
                    request_id,
                    expected_session_scope_id.clone(),
                    compaction_preparation_tx.clone(),
                    move || {
                        let store = JsonlSessionStore::new(&session_log_path)
                            .map_err(|error| format!("{error:#}"))?;
                        let session = Session::load_from_store(provider_name, model_name, store)
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

            let candidate = match pending_queued_pre_turn_preparation.take() {
                None => {
                    if next_queue_id.is_none() {
                        last_queued_pre_turn_block = None;
                    }
                    None
                }
                Some(PreTurnV2CompactionPreparation {
                    admission: QueuedConversationPreTurnAdmission::NoQueuedInput,
                    ..
                }) => {
                    last_queued_pre_turn_block = None;
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
                        if last_queued_pre_turn_block.as_ref() != Some(&block) {
                            let _ = message_tx.send(WorkerMessage::Notice(notice));
                        }
                        last_queued_pre_turn_block = Some(block);
                        Some(*candidate)
                    }
                    None => {
                        let block = (queue_id, reason);
                        if last_queued_pre_turn_block.as_ref() != Some(&block) {
                            let _ = message_tx.send(WorkerMessage::Notice(format!(
                                "queued follow-up is waiting for a local pre-turn admission: {}",
                                block.1
                            )));
                        }
                        last_queued_pre_turn_block = Some(block);
                        None
                    }
                },
                Some(PreTurnV2CompactionPreparation {
                    admission: QueuedConversationPreTurnAdmission::ExactFit(admitted),
                    ..
                }) => {
                    last_queued_pre_turn_block = None;
                    Some(admitted.candidate)
                }
                Some(PreTurnV2CompactionPreparation {
                    admission: QueuedConversationPreTurnAdmission::PortablePreflightReady(pending),
                    ..
                }) => {
                    let Some(session) = current_session.as_ref() else {
                        continue;
                    };
                    let folded_event_count = pending.folded_event_count();
                    match pending.apply_compaction(session, &current_session_log_path) {
                        Ok((candidate, outcome)) => {
                            match load_session_with_runtime_attachments(
                                session.provider_name(),
                                session.model_name(),
                                &current_session_log_path,
                                current_session.as_ref(),
                            ) {
                                Ok(reloaded) => {
                                    let entries = reloaded.entries().to_vec();
                                    current_session = Some(reloaded);
                                    last_queued_pre_turn_block = None;
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
                let committed = match current_session.as_mut() {
                    Some(session) => commit_prepared_queued_conversation_candidate(
                        &current_session_log_path,
                        session,
                        candidate,
                    ),
                    None => Err("session state is unavailable for queued promotion".to_owned()),
                };
                match committed {
                    Ok(candidate) => {
                        let provider_name = current_session
                            .as_ref()
                            .map(|session| session.provider_name().to_owned());
                        let model_name = current_session
                            .as_ref()
                            .map(|session| session.model_name().to_owned());
                        match (provider_name, model_name) {
                            (Some(provider_name), Some(model_name)) => {
                                match load_session_with_runtime_attachments(
                                    &provider_name,
                                    &model_name,
                                    &current_session_log_path,
                                    current_session.as_ref(),
                                ) {
                                    Ok(reloaded) => {
                                        current_session = Some(reloaded);
                                        exact_conversation_prompts.remove(&queue_id);
                                        if let Some(session) = current_session.as_ref() {
                                            send_conversation_queue_update(
                                                &message_tx,
                                                session.entries(),
                                            );
                                        }
                                        active_run = start_queued_conversation_run(
                                            &runtime,
                                            Arc::clone(&agent),
                                            &agent_supervisor,
                                            &root_config,
                                            agent.tool_registry(),
                                            &options,
                                            &background_agent_runs,
                                            &mut current_session,
                                            &task_result_tx,
                                            &message_tx,
                                            Arc::clone(&elicitation_handler),
                                            &mut next_run_id,
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
            if active_run.is_some() {
                continue;
            }
        }

        match command_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(WorkerCommand::QueueConversationInput {
                prompt,
                kind,
                target,
                reasoning_effort,
            }) => {
                compaction_preparation_tasks.abort_all();
                pending_queued_pre_turn_preparation = None;
                match queue_conversation_input(
                    &current_session_log_path,
                    &mut current_session,
                    &mut exact_conversation_prompts,
                    prompt,
                    kind,
                    target,
                    reasoning_effort,
                ) {
                    Ok(entries) => {
                        send_conversation_queue_update(&message_tx, &entries);
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            Ok(WorkerCommand::CancelQueuedConversationInput { queue_id }) => {
                compaction_preparation_tasks.abort_all();
                pending_queued_pre_turn_preparation = None;
                match cancel_queued_conversation_input(
                    &current_session_log_path,
                    &mut current_session,
                    &mut exact_conversation_prompts,
                    queue_id,
                ) {
                    Ok(entries) => send_conversation_queue_update(&message_tx, &entries),
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            Ok(WorkerCommand::EditQueuedConversationInput {
                queue_id,
                prompt,
                reasoning_effort,
            }) => {
                compaction_preparation_tasks.abort_all();
                pending_queued_pre_turn_preparation = None;
                match edit_queued_conversation_input(
                    &current_session_log_path,
                    &mut current_session,
                    &mut exact_conversation_prompts,
                    queue_id,
                    prompt,
                    reasoning_effort,
                ) {
                    Ok(entries) => send_conversation_queue_update(&message_tx, &entries),
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            Ok(WorkerCommand::MoveQueuedConversationInput {
                queue_id,
                direction,
            }) => {
                compaction_preparation_tasks.abort_all();
                pending_queued_pre_turn_preparation = None;
                match move_queued_conversation_input(
                    &current_session_log_path,
                    &mut current_session,
                    queue_id,
                    direction,
                ) {
                    Ok(entries) => send_conversation_queue_update(&message_tx, &entries),
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            Ok(WorkerCommand::PromoteQueuedConversationInput { queue_id }) => {
                compaction_preparation_tasks.abort_all();
                pending_queued_pre_turn_preparation = None;
                match promote_queued_conversation_input(
                    &current_session_log_path,
                    &mut current_session,
                    queue_id,
                ) {
                    Ok(entries) => send_conversation_queue_update(&message_tx, &entries),
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            Ok(WorkerCommand::SendQueuedConversationInputNow { queue_id }) => {
                compaction_preparation_tasks.abort_all();
                pending_queued_pre_turn_preparation = None;
                match promote_queued_conversation_input(
                    &current_session_log_path,
                    &mut current_session,
                    queue_id,
                ) {
                    Ok(entries) => {
                        send_conversation_queue_update(&message_tx, &entries);
                        if let Some(run) = active_run.take() {
                            cancel_active_run(
                                run,
                                &runtime,
                                &root_config,
                                &current_session_log_path,
                                &mut current_session,
                                &message_tx,
                                &elicitation_handler,
                                &agent_supervisor,
                                &mut discarded_run_ids,
                                "run interrupted for follow-up",
                            );
                        }
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            Ok(WorkerCommand::SetConversationQueuePaused { paused }) => {
                match set_conversation_queue_paused(
                    &current_session_log_path,
                    &mut current_session,
                    paused,
                ) {
                    Ok(entries) => send_conversation_queue_update(&message_tx, &entries),
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            Ok(
                command @ (WorkerCommand::SubmitPrompt { .. }
                | WorkerCommand::SubmitPromptWithAttachments { .. }
                | WorkerCommand::SubmitPlanPrompt { .. }),
            ) => {
                let (prompt, attachments, reasoning_effort, plan_mode) = match command {
                    WorkerCommand::SubmitPrompt {
                        prompt,
                        reasoning_effort,
                    } => (prompt, Vec::new(), reasoning_effort, false),
                    WorkerCommand::SubmitPromptWithAttachments {
                        prompt,
                        attachments,
                        reasoning_effort,
                    } => (prompt, attachments, reasoning_effort, false),
                    WorkerCommand::SubmitPlanPrompt {
                        prompt,
                        reasoning_effort,
                    } => (prompt, Vec::new(), reasoning_effort, true),
                    _ => unreachable!("matched submit prompt commands above"),
                };
                if active_run.is_some() {
                    if !attachments.is_empty() {
                        let _ = message_tx.send(WorkerMessage::RunFailed(
                            "image attachments cannot be queued; wait for the active run"
                                .to_owned(),
                        ));
                        continue;
                    }
                    let kind = if plan_mode {
                        ConversationInputKind::PlanPrompt
                    } else {
                        ConversationInputKind::Chat
                    };
                    match queue_conversation_input(
                        &current_session_log_path,
                        &mut current_session,
                        &mut exact_conversation_prompts,
                        prompt,
                        kind,
                        ConversationInputTarget::MainThread,
                        reasoning_effort,
                    ) {
                        Ok(entries) => send_conversation_queue_update(&message_tx, &entries),
                        Err(error) => {
                            let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        }
                    }
                    continue;
                }

                let Some(run_session) = current_session.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };

                let safe_started_prompt = if prompt.is_empty() && !attachments.is_empty() {
                    sigil_kernel::render_image_attachment_placeholders(&attachments)
                } else {
                    sigil_kernel::safe_persistence_text(&prompt)
                };
                let started = if plan_mode {
                    WorkerMessage::PlanRunStarted {
                        prompt: safe_started_prompt,
                    }
                } else {
                    WorkerMessage::RunStarted {
                        prompt: safe_started_prompt,
                    }
                };
                let _ = message_tx.send(started);

                let mut handler = ChannelEventHandler::new(message_tx.clone());
                let (approval_tx, approval_rx) = mpsc::channel();
                let elicitation_audit_buffer: McpElicitationAuditBuffer =
                    Arc::new(std::sync::Mutex::new(Vec::new()));
                elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
                let agent = Arc::clone(&agent);
                let mut options = options.clone();
                options.reasoning_effort = Some(reasoning_effort);
                let mut agent_delegate = sigil_runtime::AgentToolRuntime::new(
                    agent_supervisor.clone(),
                    root_config.clone(),
                    agent.tool_registry().clone(),
                )
                .with_background_runs(background_agent_runs.clone());
                let plan_tools = plan_mode.then(|| {
                    sigil_runtime::build_plan_prompt_tool_registry(
                        agent.tool_registry(),
                        &root_config,
                    )
                    .into_registry()
                });
                let task_result_tx = task_result_tx.clone();
                let run_id = next_run_id;
                next_run_id += 1;
                let provider_logical_run_id = format!("foreground-run-{run_id}");
                let context_resolver = context_resolver.clone();
                let cancellation_recorder = match run_session.run_cancellation_recorder() {
                    Ok(recorder) => recorder,
                    Err(error) => {
                        current_session = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                            "failed to create cancellation recorder: {error}"
                        )));
                        continue;
                    }
                };
                let cancellation_owner = RunCancellationOwner::new();
                let cancellation_handle = cancellation_owner.handle();
                let run_task_guard = cancellation_handle
                    .register_task()
                    .expect("new root cancellation owner must admit its first task");

                let url_capability_registrar = run_session.user_url_capability_registrar();
                let image_attachment_resolver = run_session.image_attachment_resolver();
                let handle = runtime.spawn(async move {
                    let _run_task_guard = run_task_guard;
                    let mut run_session = run_session;
                    let result = {
                        let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
                        let input = chat_agent_run_input_with_repo_context(
                            &context_resolver,
                            prompt,
                            plan_mode,
                            Vec::new(),
                        )
                        .await
                        .with_image_attachments(attachments)
                        .with_logical_run_id(provider_logical_run_id.clone())
                        .with_cancellation(cancellation_handle);
                        if let Some(tools) = plan_tools {
                            agent
                                .run_with_approval_input_tool_registry_and_agent_delegate(
                                    &mut run_session,
                                    input,
                                    options,
                                    tools,
                                    &mut handler,
                                    &mut approval_handler,
                                    &mut agent_delegate,
                                )
                                .await
                        } else {
                            agent
                                .run_with_approval_input_and_agent_delegate(
                                    &mut run_session,
                                    input,
                                    options,
                                    &mut handler,
                                    &mut approval_handler,
                                    &mut agent_delegate,
                                )
                                .await
                        }
                        .map(|output| output.result)
                        .map_err(|error| format!("{error:#}"))
                    };
                    let result = match append_mcp_elicitation_audits(
                        &mut run_session,
                        &run_elicitation_audit_buffer,
                    ) {
                        Ok(()) => result,
                        Err(error) => Err(error),
                    };
                    let _ = task_result_tx.send(RunTaskResult {
                        run_id,
                        session: run_session,
                        payload: RunTaskPayload::Chat {
                            result,
                            plan_mode,
                            queue_id: None,
                            provider_logical_run_id: Some(provider_logical_run_id),
                            agent_result_continuation_thread_ids: Vec::new(),
                        },
                    });
                });

                active_run = Some(ActiveRun {
                    run_id,
                    handle,
                    approval_tx,
                    elicitation_audit_buffer,
                    cancellation_owner,
                    cancellation_recorder,
                    url_capability_registrar,
                    image_attachment_resolver,
                });
            }
            Ok(WorkerCommand::InvokeAgentProfile {
                profile_id,
                prompt,
                parent_prompt,
            }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "agent is already running".to_owned(),
                    ));
                    continue;
                }

                let Some(run_session) = current_session.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };
                let mut run_session = run_session;
                let safe_parent_prompt = sigil_kernel::safe_persistence_text(&parent_prompt);
                if let Err(error) =
                    run_session.append_user_message(ModelMessage::user(safe_parent_prompt.clone()))
                {
                    let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                        "failed to persist agent invocation prompt: {error:#}"
                    )));
                    current_session = Some(run_session);
                    continue;
                }

                let _ = message_tx.send(WorkerMessage::AgentRunStarted {
                    profile_id: profile_id.clone(),
                    prompt: safe_parent_prompt,
                });

                let mut handler = ChannelEventHandler::new(message_tx.clone());
                let (approval_tx, approval_rx) = mpsc::channel();
                let elicitation_audit_buffer: McpElicitationAuditBuffer =
                    Arc::new(std::sync::Mutex::new(Vec::new()));
                elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
                let mut agent_delegate = sigil_runtime::AgentToolRuntime::new(
                    agent_supervisor.clone(),
                    root_config.clone(),
                    agent.tool_registry().clone(),
                )
                .with_background_runs(background_agent_runs.clone());
                let options = options.clone();
                let task_result_tx = task_result_tx.clone();
                let run_id = next_run_id;
                next_run_id += 1;
                let cancellation_recorder = match run_session.run_cancellation_recorder() {
                    Ok(recorder) => recorder,
                    Err(error) => {
                        current_session = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                            "failed to create cancellation recorder: {error}"
                        )));
                        continue;
                    }
                };
                let cancellation_owner = RunCancellationOwner::new();
                let cancellation_handle = cancellation_owner.handle();
                let run_task_guard = cancellation_handle
                    .register_task()
                    .expect("new root cancellation owner must admit its first task");
                sigil_kernel::AgentToolDelegate::set_run_cancellation(
                    &mut agent_delegate,
                    Some(cancellation_handle.clone()),
                );

                let url_capability_registrar = run_session.user_url_capability_registrar();
                let image_attachment_resolver = run_session.image_attachment_resolver();
                let handle = runtime.spawn(async move {
                    let _run_task_guard = run_task_guard;
                    let profile_id_for_summary = profile_id.clone();
                    let terminal_cancellation = cancellation_handle.clone();
                    let result = async {
                        let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
                        match AgentProfileId::new(profile_id.clone()) {
                            Ok(profile_id_value) => agent_delegate
                                .invoke_agent_profile(
                                    &mut run_session,
                                    profile_id_value,
                                    prompt,
                                    &options,
                                    &mut handler,
                                    &mut approval_handler,
                                )
                                .await
                                .and_then(|invocation| {
                                    let run_result = manual_agent_invocation_result(&invocation);
                                    run_session.append_assistant_message(
                                        ModelMessage::assistant(
                                            Some(manual_agent_parent_summary(
                                                &profile_id_for_summary,
                                                &invocation,
                                            )),
                                            Vec::new(),
                                        ),
                                    )?;
                                    Ok(run_result)
                                })
                                .map_err(|error| format!("{error:#}")),
                            Err(error) => Err(format!("{error:#}")),
                        }
                    };
                    let result = result.await;
                    let result = if terminal_cancellation.try_finalize_naturally() {
                        result
                    } else {
                        Err("run cancellation won the manual agent terminal-state race".to_owned())
                    };
                    let result = match append_mcp_elicitation_audits(
                        &mut run_session,
                        &run_elicitation_audit_buffer,
                    ) {
                        Ok(()) => result,
                        Err(error) => Err(error),
                    };
                    let _ = task_result_tx.send(RunTaskResult {
                        run_id,
                        session: run_session,
                        payload: RunTaskPayload::Agent { profile_id, result },
                    });
                });

                active_run = Some(ActiveRun {
                    run_id,
                    handle,
                    approval_tx,
                    elicitation_audit_buffer,
                    cancellation_owner,
                    cancellation_recorder,
                    url_capability_registrar,
                    image_attachment_resolver,
                });
            }
            Ok(WorkerCommand::InvokeInlineSkill {
                skill_id,
                arguments,
                reasoning_effort,
            }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "agent is already running".to_owned(),
                    ));
                    continue;
                }

                let run_id = next_run_id;
                let loaded =
                    match load_worker_skill(&root_config, &options, &skill_id, Some(run_id)) {
                        Ok(loaded) => loaded,
                        Err(error) => {
                            let _ = message_tx.send(WorkerMessage::RunFailed(error));
                            continue;
                        }
                    };
                if loaded.descriptor.run_as != SkillRunMode::Inline {
                    let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                        "agent {skill_id} is configured for {} mode, not inline skill mode",
                        loaded.descriptor.run_as.as_str()
                    )));
                    continue;
                }
                let Some(run_session) = current_session.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };

                let prompt = skill_invocation_prompt(&skill_id, &arguments);
                let _ = message_tx.send(WorkerMessage::SkillRunStarted {
                    skill_id: skill_id.clone(),
                    prompt: sigil_kernel::safe_persistence_text(&prompt),
                });

                let mut handler = ChannelEventHandler::new(message_tx.clone());
                let (approval_tx, approval_rx) = mpsc::channel();
                let elicitation_audit_buffer: McpElicitationAuditBuffer =
                    Arc::new(std::sync::Mutex::new(Vec::new()));
                elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
                let skill_registry = sigil_runtime::build_skill_tool_registry(
                    agent.tool_registry(),
                    &loaded.descriptor,
                )
                .into_registry();
                let agent = Arc::clone(&agent);
                let mut options = options.clone();
                options.reasoning_effort = Some(reasoning_effort);
                let task_result_tx = task_result_tx.clone();
                let run_id = next_run_id;
                next_run_id += 1;
                let cancellation_recorder = match run_session.run_cancellation_recorder() {
                    Ok(recorder) => recorder,
                    Err(error) => {
                        current_session = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                            "failed to create cancellation recorder: {error}"
                        )));
                        continue;
                    }
                };
                let cancellation_owner = RunCancellationOwner::new();
                let cancellation_handle = cancellation_owner.handle();
                let run_task_guard = cancellation_handle
                    .register_task()
                    .expect("new root cancellation owner must admit its first task");

                let url_capability_registrar = run_session.user_url_capability_registrar();
                let image_attachment_resolver = run_session.image_attachment_resolver();
                let handle = runtime.spawn(async move {
                    let _run_task_guard = run_task_guard;
                    let mut run_session = run_session;
                    let input = AgentRunInput::transient(prompt, vec![loaded.transient_context])
                        .with_cancellation(cancellation_handle);
                    let result =
                        match run_session.append_control(ControlEntry::SkillLoaded(loaded.entry)) {
                            Ok(()) => {
                                let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
                                agent
                                    .run_with_approval_input_and_tool_registry(
                                        &mut run_session,
                                        input,
                                        options,
                                        skill_registry,
                                        &mut handler,
                                        &mut approval_handler,
                                    )
                                    .await
                                    .map(|output| output.result)
                                    .map_err(|error| format!("{error:#}"))
                            }
                            Err(error) => Err(format!("{error:#}")),
                        };
                    let result = match append_mcp_elicitation_audits(
                        &mut run_session,
                        &run_elicitation_audit_buffer,
                    ) {
                        Ok(()) => result,
                        Err(error) => Err(error),
                    };
                    let _ = task_result_tx.send(RunTaskResult {
                        run_id,
                        session: run_session,
                        payload: RunTaskPayload::Chat {
                            result,
                            plan_mode: false,
                            queue_id: None,
                            provider_logical_run_id: None,
                            agent_result_continuation_thread_ids: Vec::new(),
                        },
                    });
                });

                active_run = Some(ActiveRun {
                    run_id,
                    handle,
                    approval_tx,
                    elicitation_audit_buffer,
                    cancellation_owner,
                    cancellation_recorder,
                    url_capability_registrar,
                    image_attachment_resolver,
                });
            }
            Ok(WorkerCommand::InvokeChildSessionSkill {
                skill_id,
                arguments,
            }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "agent is already running".to_owned(),
                    ));
                    continue;
                }
                if !root_config.task.enabled {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "task planning is disabled in config".to_owned(),
                    ));
                    continue;
                }

                let run_id = next_run_id;
                let loaded =
                    match load_worker_skill(&root_config, &options, &skill_id, Some(run_id)) {
                        Ok(loaded) => loaded,
                        Err(error) => {
                            let _ = message_tx.send(WorkerMessage::RunFailed(error));
                            continue;
                        }
                    };
                if loaded.descriptor.run_as != SkillRunMode::ChildSession {
                    let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                        "skill {skill_id} is configured for {} mode, not agent mode",
                        loaded.descriptor.run_as.as_str()
                    )));
                    continue;
                }
                let Some(run_session) = current_session.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };

                let task_id = match next_task_id(&run_session) {
                    Ok(task_id) => task_id,
                    Err(error) => {
                        current_session = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };
                let task_id_value = task_id.as_str().to_owned();
                let parent_session_ref = match session_ref_for_log_path(&current_session_log_path) {
                    Ok(reference) => reference,
                    Err(error) => {
                        current_session = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };
                let objective = skill_child_session_objective(&skill_id, &arguments);
                let _ = message_tx.send(WorkerMessage::TaskRunStarted {
                    task_id: task_id_value.clone(),
                    objective: sigil_kernel::safe_persistence_text(&objective),
                });

                let handler = ChannelEventHandler::new(message_tx.clone());
                let (approval_tx, approval_rx) = mpsc::channel();
                let elicitation_audit_buffer: McpElicitationAuditBuffer =
                    Arc::new(std::sync::Mutex::new(Vec::new()));
                elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
                let run_id = next_run_id;
                next_run_id += 1;
                let (
                    cancellation_owner,
                    cancellation_recorder,
                    cancellation_handle,
                    cancellation_task_guard,
                ) = match prepare_run_cancellation(&run_session) {
                    Ok(cancellation) => cancellation,
                    Err(error) => {
                        current_session = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };

                let url_capability_registrar = run_session.user_url_capability_registrar();
                let image_attachment_resolver = run_session.image_attachment_resolver();
                let handle = spawn_skill_child_run(
                    &runtime,
                    SkillChildRunSpawn {
                        run_id,
                        session: run_session,
                        task_id,
                        task_id_value,
                        parent_session_ref,
                        objective,
                        skill_id,
                        arguments,
                        loaded,
                        root_config: root_config.clone(),
                        options: options.clone(),
                        base_registry: agent.tool_registry().clone(),
                        agent_supervisor: agent_supervisor.clone(),
                        role_provider_builder: Arc::clone(&role_provider_builder),
                        task_result_tx: task_result_tx.clone(),
                        approval_rx,
                        handler,
                        elicitation_audit_buffer: run_elicitation_audit_buffer,
                        cancellation_handle,
                        cancellation_task_guard,
                    },
                );

                active_run = Some(ActiveRun {
                    run_id,
                    handle,
                    approval_tx,
                    elicitation_audit_buffer,
                    cancellation_owner,
                    cancellation_recorder,
                    url_capability_registrar,
                    image_attachment_resolver,
                });
            }
            Ok(WorkerCommand::SubmitTask { prompt }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "agent is already running".to_owned(),
                    ));
                    continue;
                }
                if !root_config.task.enabled {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "task planning is disabled in config".to_owned(),
                    ));
                    continue;
                }

                let Some(run_session) = current_session.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };

                let task_id = match next_task_id(&run_session) {
                    Ok(task_id) => task_id,
                    Err(error) => {
                        current_session = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };
                let task_id_value = task_id.as_str().to_owned();
                let parent_session_ref = match session_ref_for_log_path(&current_session_log_path) {
                    Ok(reference) => reference,
                    Err(error) => {
                        current_session = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };
                let _ = message_tx.send(WorkerMessage::TaskRunStarted {
                    task_id: task_id_value.clone(),
                    objective: sigil_kernel::safe_persistence_text(&prompt),
                });

                let handler = ChannelEventHandler::new(message_tx.clone());
                let (approval_tx, approval_rx) = mpsc::channel();
                let elicitation_audit_buffer: McpElicitationAuditBuffer =
                    Arc::new(std::sync::Mutex::new(Vec::new()));
                elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
                let run_id = next_run_id;
                next_run_id += 1;
                let (
                    cancellation_owner,
                    cancellation_recorder,
                    cancellation_handle,
                    cancellation_task_guard,
                ) = match prepare_run_cancellation(&run_session) {
                    Ok(cancellation) => cancellation,
                    Err(error) => {
                        current_session = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };

                let url_capability_registrar = run_session.user_url_capability_registrar();
                let image_attachment_resolver = run_session.image_attachment_resolver();
                let handle = spawn_task_run(
                    &runtime,
                    TaskRunSpawn {
                        run_id,
                        session: run_session,
                        task_id,
                        task_id_value,
                        parent_session_ref,
                        objective: prompt,
                        root_config: root_config.clone(),
                        options: options.clone(),
                        base_registry: agent.tool_registry().clone(),
                        agent_supervisor: agent_supervisor.clone(),
                        role_provider_builder: Arc::clone(&role_provider_builder),
                        task_result_tx: task_result_tx.clone(),
                        approval_rx,
                        handler,
                        elicitation_audit_buffer: run_elicitation_audit_buffer,
                        cancellation_handle,
                        cancellation_task_guard,
                    },
                );

                active_run = Some(ActiveRun {
                    run_id,
                    handle,
                    approval_tx,
                    elicitation_audit_buffer,
                    cancellation_owner,
                    cancellation_recorder,
                    url_capability_registrar,
                    image_attachment_resolver,
                });
            }
            Ok(WorkerCommand::ContinueTask { task_id, guidance }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "agent is already running".to_owned(),
                    ));
                    continue;
                }
                if !root_config.task.enabled {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "task planning is disabled in config".to_owned(),
                    ));
                    continue;
                }

                let Some(run_session) = current_session.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };
                let (task_id, task_id_value, objective) =
                    match resolve_continue_task(&run_session, task_id) {
                        Ok(resolved) => resolved,
                        Err(error) => {
                            current_session = Some(run_session);
                            let _ = message_tx.send(WorkerMessage::RunFailed(error));
                            continue;
                        }
                    };
                let parent_session_ref = match session_ref_for_log_path(&current_session_log_path) {
                    Ok(reference) => reference,
                    Err(error) => {
                        current_session = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };
                let _ = message_tx.send(WorkerMessage::TaskRunStarted {
                    task_id: task_id_value.clone(),
                    objective: sigil_kernel::safe_persistence_text(&objective),
                });

                let handler = ChannelEventHandler::new(message_tx.clone());
                let (approval_tx, approval_rx) = mpsc::channel();
                let elicitation_audit_buffer: McpElicitationAuditBuffer =
                    Arc::new(std::sync::Mutex::new(Vec::new()));
                elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
                let run_id = next_run_id;
                next_run_id += 1;
                let (
                    cancellation_owner,
                    cancellation_recorder,
                    cancellation_handle,
                    cancellation_task_guard,
                ) = match prepare_run_cancellation(&run_session) {
                    Ok(cancellation) => cancellation,
                    Err(error) => {
                        current_session = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };

                let url_capability_registrar = run_session.user_url_capability_registrar();
                let image_attachment_resolver = run_session.image_attachment_resolver();
                let handle = spawn_task_continue(
                    &runtime,
                    TaskContinueSpawn {
                        run_id,
                        session: run_session,
                        task_id,
                        task_id_value,
                        parent_session_ref,
                        objective,
                        guidance,
                        root_config: root_config.clone(),
                        options: options.clone(),
                        base_registry: agent.tool_registry().clone(),
                        agent_supervisor: agent_supervisor.clone(),
                        role_provider_builder: Arc::clone(&role_provider_builder),
                        task_result_tx: task_result_tx.clone(),
                        approval_rx,
                        handler,
                        elicitation_audit_buffer: run_elicitation_audit_buffer,
                        cancellation_handle,
                        cancellation_task_guard,
                    },
                );

                active_run = Some(ActiveRun {
                    run_id,
                    handle,
                    approval_tx,
                    elicitation_audit_buffer,
                    cancellation_owner,
                    cancellation_recorder,
                    url_capability_registrar,
                    image_attachment_resolver,
                });
            }
            Ok(WorkerCommand::ApprovalDecision { call_id, approved }) => {
                if let Some(active_run) = &active_run {
                    let approval = if approved {
                        ToolApproval::Approve
                    } else {
                        ToolApproval::Deny {
                            reason: "denied in TUI".to_owned(),
                        }
                    };
                    let _ = active_run
                        .approval_tx
                        .send(ApprovalSignal::Decision { call_id, approval });
                } else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "received stray approval decision without pending approval".to_owned(),
                    ));
                }
            }
            Ok(WorkerCommand::ApprovalDecisionWithArgs { call_id, args_json }) => {
                if let Some(active_run) = &active_run {
                    let _ = active_run.approval_tx.send(ApprovalSignal::Decision {
                        call_id,
                        approval: ToolApproval::ApproveWithArgs { args_json },
                    });
                } else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "received stray approval decision without pending approval".to_owned(),
                    ));
                }
            }
            Ok(WorkerCommand::ApprovalSessionDecision { call_id }) => {
                if let Some(active_run) = &active_run {
                    let _ = active_run.approval_tx.send(ApprovalSignal::Decision {
                        call_id,
                        approval: ToolApproval::ApproveForSession,
                    });
                } else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "received stray approval decision without pending approval".to_owned(),
                    ));
                }
            }
            Ok(WorkerCommand::ApprovalCommand(command)) => {
                if processed_worker_command_ids.contains(&command.command_id) {
                    let _ = message_tx.send(WorkerMessage::Notice(format!(
                        "duplicate command {} ignored",
                        command.command_id
                    )));
                    continue;
                }
                let signal = match command.payload {
                    WorkerApprovalCommand::Decision { call_id, approved } => {
                        let approval = if approved {
                            ToolApproval::Approve
                        } else {
                            ToolApproval::Deny {
                                reason: "denied in TUI".to_owned(),
                            }
                        };
                        ApprovalSignal::Decision { call_id, approval }
                    }
                    WorkerApprovalCommand::DecisionForSession { call_id } => {
                        ApprovalSignal::Decision {
                            call_id,
                            approval: ToolApproval::ApproveForSession,
                        }
                    }
                    WorkerApprovalCommand::DecisionWithArgs { call_id, args_json } => {
                        ApprovalSignal::Decision {
                            call_id,
                            approval: ToolApproval::ApproveWithArgs { args_json },
                        }
                    }
                };
                if let Some(active_run) = &active_run {
                    if active_run.approval_tx.send(signal).is_ok() {
                        processed_worker_command_ids.insert(command.command_id);
                    } else {
                        let _ = message_tx.send(WorkerMessage::RunFailed(
                            "approval channel closed".to_owned(),
                        ));
                    }
                } else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "received stray approval command without pending approval".to_owned(),
                    ));
                }
            }
            Ok(WorkerCommand::BackgroundActiveAgent) => {
                if active_run.is_none() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "no active agent run to background".to_owned(),
                    ));
                    continue;
                }
                match agent_supervisor.request_foreground_background() {
                    Ok(thread_id) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "agent {} background requested",
                            thread_id.as_str()
                        )));
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "agent background unavailable: {error}"
                        )));
                    }
                }
            }
            Ok(WorkerCommand::CancelRun) => {
                if let Some(active_run) = active_run.take() {
                    cancel_active_run(
                        active_run,
                        &runtime,
                        &root_config,
                        &current_session_log_path,
                        &mut current_session,
                        &message_tx,
                        &elicitation_handler,
                        &agent_supervisor,
                        &mut discarded_run_ids,
                        "run cancelled from TUI",
                    );
                } else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "no active run to cancel".to_owned(),
                    ));
                }
            }
            Ok(WorkerCommand::CancelTerminalTask { task_id }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before cancelling terminal task".to_owned(),
                    ));
                    continue;
                }
                match cancel_terminal_task(
                    &runtime,
                    agent.tool_registry().clone(),
                    &root_config,
                    &options,
                    &current_session_log_path,
                    &mut current_session,
                    task_id,
                ) {
                    Ok((entry, entries)) => {
                        let _ =
                            message_tx.send(WorkerMessage::TerminalTaskUpdated { entry, entries });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(error));
                    }
                }
            }
            Ok(WorkerCommand::CreateTaskFromPlan {
                plan_id,
                expected_plan_hash,
                start_mode,
                permission_grant,
            }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before creating a task from a plan".to_owned(),
                    ));
                    continue;
                }
                if !root_config.task.enabled {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "task planning is disabled in config".to_owned(),
                    ));
                    continue;
                }
                let created = match create_task_from_plan(
                    &root_config,
                    &workspace_root,
                    &current_session_log_path,
                    &mut current_session,
                    CreateTaskFromPlanRequest {
                        plan_id,
                        expected_plan_hash,
                        start_mode,
                        permission_grant,
                    },
                ) {
                    Ok(created) => created,
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(error));
                        continue;
                    }
                };
                let _ = message_tx.send(WorkerMessage::TaskCreatedFromPlan {
                    entry: created.entry.clone(),
                    start_mode: created.start_mode,
                    entries: created.entries.clone(),
                });
                if created.start_mode == PlanTaskStartMode::CreatePaused {
                    continue;
                }

                let Some(run_session) = current_session.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };
                let parent_session_ref = match session_ref_for_log_path(&current_session_log_path) {
                    Ok(reference) => reference,
                    Err(error) => {
                        current_session = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };
                let _ = message_tx.send(WorkerMessage::TaskRunStarted {
                    task_id: created.task_id_value.clone(),
                    objective: sigil_kernel::safe_persistence_text(&created.objective),
                });
                let handler = ChannelEventHandler::new(message_tx.clone());
                let (approval_tx, approval_rx) = mpsc::channel();
                let elicitation_audit_buffer: McpElicitationAuditBuffer =
                    Arc::new(std::sync::Mutex::new(Vec::new()));
                elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
                let run_id = next_run_id;
                next_run_id += 1;
                let (
                    cancellation_owner,
                    cancellation_recorder,
                    cancellation_handle,
                    cancellation_task_guard,
                ) = match prepare_run_cancellation(&run_session) {
                    Ok(cancellation) => cancellation,
                    Err(error) => {
                        current_session = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };
                let url_capability_registrar = run_session.user_url_capability_registrar();
                let image_attachment_resolver = run_session.image_attachment_resolver();
                let handle = spawn_task_run(
                    &runtime,
                    TaskRunSpawn {
                        run_id,
                        session: run_session,
                        task_id: created.task_id,
                        task_id_value: created.task_id_value,
                        parent_session_ref,
                        objective: created.objective,
                        root_config: root_config.clone(),
                        options: options.clone(),
                        base_registry: agent.tool_registry().clone(),
                        agent_supervisor: agent_supervisor.clone(),
                        role_provider_builder: Arc::clone(&role_provider_builder),
                        task_result_tx: task_result_tx.clone(),
                        approval_rx,
                        handler,
                        elicitation_audit_buffer: run_elicitation_audit_buffer,
                        cancellation_handle,
                        cancellation_task_guard,
                    },
                );
                active_run = Some(ActiveRun {
                    run_id,
                    handle,
                    approval_tx,
                    elicitation_audit_buffer,
                    cancellation_owner,
                    cancellation_recorder,
                    url_capability_registrar,
                    image_attachment_resolver,
                });
            }
            Ok(WorkerCommand::RejectPlan {
                plan_id,
                expected_plan_hash,
            }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before rejecting a plan".to_owned(),
                    ));
                    continue;
                }
                match reject_plan(
                    &root_config,
                    &current_session_log_path,
                    &mut current_session,
                    RejectPlanRequest {
                        plan_id,
                        expected_plan_hash,
                    },
                ) {
                    Ok((entry, entries)) => {
                        let _ = message_tx.send(WorkerMessage::PlanRejected { entry, entries });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(error));
                    }
                }
            }
            Ok(WorkerCommand::ApprovePlan {
                plan_text,
                permission,
                scope_summary,
                clear_planning_context,
            }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before approving a plan".to_owned(),
                    ));
                    continue;
                }
                match approve_plan(
                    &root_config,
                    &current_session_log_path,
                    &mut current_session,
                    PlanApprovalRequest {
                        plan_text,
                        permission,
                        scope_summary,
                        clear_planning_context,
                    },
                ) {
                    Ok((entry, entries)) => {
                        let _ = message_tx.send(WorkerMessage::PlanApproved { entry, entries });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(error));
                    }
                }
            }
            Ok(WorkerCommand::CloseAgent { thread_id, reason }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before closing agent".to_owned(),
                    ));
                    continue;
                }
                match close_agent_thread(
                    &root_config,
                    &current_session_log_path,
                    &mut current_session,
                    thread_id,
                    reason,
                ) {
                    Ok((thread_id, entries)) => {
                        let _ = message_tx
                            .send(WorkerMessage::AgentThreadClosed { thread_id, entries });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(error));
                    }
                }
            }
            Ok(WorkerCommand::CancelAgent { thread_id, reason }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before cancelling agent".to_owned(),
                    ));
                    continue;
                }
                match cancel_agent_thread(
                    &runtime,
                    &background_agent_runs,
                    &agent_supervisor,
                    &root_config,
                    agent.tool_registry(),
                    &options,
                    &mut current_session,
                    thread_id,
                    reason,
                ) {
                    Ok((thread_id, entries)) => {
                        let _ = message_tx
                            .send(WorkerMessage::AgentThreadCancelled { thread_id, entries });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(error));
                    }
                }
            }
            Ok(WorkerCommand::MessageAgent { thread_id, prompt }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before messaging agent".to_owned(),
                    ));
                    continue;
                }
                match message_agent_thread(
                    &runtime,
                    &background_agent_runs,
                    &agent_supervisor,
                    &root_config,
                    agent.tool_registry(),
                    &options,
                    &mut current_session,
                    thread_id,
                    prompt,
                ) {
                    Ok((mut result, controls)) => {
                        for control in controls {
                            let _ = message_tx
                                .send(WorkerMessage::Event(Box::new(RunEvent::Control(control))));
                        }
                        result.control_entries.clear();
                        let _ = message_tx
                            .send(WorkerMessage::Event(Box::new(RunEvent::ToolResult(result))));
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(error));
                    }
                }
            }
            Ok(WorkerCommand::PreviewV2Compaction) => {
                pending_v2_compaction = None;
                pending_queued_pre_turn_preparation = None;
                compaction_preparation_tasks.abort_all();
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "cannot preview compaction while the agent is running".to_owned(),
                    ));
                    continue;
                }
                let Some(session) = current_session.as_ref() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };
                let effective_config = effective_compaction_config(
                    session.provider_name(),
                    session.model_name(),
                    &options.compaction_config,
                );
                if !effective_config.enabled {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "compaction is disabled".to_owned(),
                    ));
                    continue;
                }
                match session.v2_compaction_preview(effective_config.tail_messages) {
                    Ok(Some(preview)) => {
                        let request_id = next_v2_compaction_request_id;
                        next_v2_compaction_request_id =
                            next_v2_compaction_request_id.saturating_add(1);
                        let expected_session_scope_id = session.session_scope_id().to_owned();
                        let provider_name = session.provider_name().to_owned();
                        let model_name = session.model_name().to_owned();
                        let root_config = root_config.clone();
                        let workspace_root = workspace_root.clone();
                        let session_log_path = current_session_log_path.clone();
                        let options = options.clone();
                        let tools = agent.tool_registry().specs();
                        let runtime_handle = runtime.handle().clone();
                        let manual_context_resolver = context_resolver.clone();
                        compaction_preparation_tasks.start_manual(
                            &runtime,
                            request_id,
                            expected_session_scope_id.clone(),
                            compaction_preparation_tx.clone(),
                            move || {
                                let store = JsonlSessionStore::new(&session_log_path)
                                    .map_err(|error| format!("{error:#}"))?;
                                let session =
                                    Session::load_from_store(provider_name, model_name, store)
                                        .map_err(|error| format!("{error:#}"))?;
                                if session.session_scope_id() != expected_session_scope_id {
                                    return Err(
                                        "V2 compaction preparation loaded a different session scope"
                                            .to_owned(),
                                    );
                                }
                                let (review, pending) = prepare_v2_compaction_review(
                                    request_id,
                                    &root_config,
                                    &workspace_root,
                                    &session_log_path,
                                    &session,
                                    &options,
                                    tools,
                                    &manual_context_resolver,
                                    &runtime_handle,
                                    preview,
                                )
                                .map_err(|error| format!("{error:#}"))?;
                                Ok(ManualV2CompactionPreparation { review, pending })
                            },
                        );
                    }
                    Ok(None) => {
                        let durable_message_count = session
                            .entries()
                            .iter()
                            .filter(|entry| {
                                matches!(
                                    entry,
                                    SessionLogEntry::User(_)
                                        | SessionLogEntry::Assistant(_)
                                        | SessionLogEntry::ToolResult(_)
                                )
                            })
                            .count();
                        let _ = message_tx.send(WorkerMessage::V2CompactionPreviewed {
                            state: V2CompactionPreviewState::NoFoldableHistory {
                                durable_message_count,
                                configured_tail_message_count: effective_config.tail_messages,
                            },
                        });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                            "V2 compaction preview failed: {error:#}"
                        )));
                    }
                }
            }
            Ok(WorkerCommand::ApplyV2Compaction { request_id }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                        request_id,
                        error: "cannot apply compaction while the agent is running".to_owned(),
                    });
                    continue;
                }
                let Some(pending) = pending_v2_compaction.take() else {
                    let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                        request_id,
                        error: "no admitted V2 compaction review is pending".to_owned(),
                    });
                    continue;
                };
                if pending.request_id() != request_id {
                    let reviewed_request_id = pending.request_id();
                    pending_v2_compaction = Some(pending);
                    let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                        request_id,
                        error: format!(
                            "stale V2 compaction confirmation (review request is {reviewed_request_id})"
                        ),
                    });
                    continue;
                }
                let Some(session) = current_session.as_ref() else {
                    let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                        request_id,
                        error: "session state is unavailable".to_owned(),
                    });
                    continue;
                };
                let provider_name = session.provider_name().to_owned();
                let model_name = session.model_name().to_owned();
                let folded_event_count = pending.folded_event_count();
                let applied = pending.apply(session, &current_session_log_path);
                match applied {
                    Ok(outcome) => {
                        let reloaded = load_session_with_runtime_attachments(
                            &provider_name,
                            &model_name,
                            &current_session_log_path,
                            current_session.as_ref(),
                        );
                        let entries = match reloaded {
                            Ok(session) => {
                                let entries = session.entries().to_vec();
                                current_session = Some(session);
                                entries
                            }
                            Err(error) => {
                                let _ = message_tx.send(WorkerMessage::Notice(format!(
                                    "compaction applied, but session reload was deferred: {error:#}"
                                )));
                                current_session
                                    .as_ref()
                                    .map(|current| current.entries().to_vec())
                                    .unwrap_or_default()
                            }
                        };
                        let _ = message_tx.send(WorkerMessage::V2CompactionApplied {
                            request_id,
                            source: V2CompactionApplySource::ManualConfirmation,
                            compaction_id: outcome.compaction_id,
                            folded_event_count,
                            entries,
                        });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                            request_id,
                            error: format!("{error:#}"),
                        });
                    }
                }
            }
            Ok(WorkerCommand::CancelV2CompactionReview { request_id }) => {
                let preparation_cancelled = compaction_preparation_tasks.cancel(request_id);
                if pending_v2_compaction
                    .as_ref()
                    .is_some_and(|pending| pending.request_id() == request_id)
                {
                    pending_v2_compaction = None;
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "discarded pending V2 compaction review".to_owned(),
                    ));
                } else if preparation_cancelled {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "cancelled V2 compaction preparation".to_owned(),
                    ));
                }
            }
            Ok(WorkerCommand::CheckChangedFilesDiagnostics) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "cannot check changes while the agent is running".to_owned(),
                    ));
                    continue;
                }
                let changed_paths = match changed_source_files(&options.workspace_root) {
                    Ok(paths) => paths,
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                        continue;
                    }
                };
                if changed_paths.is_empty() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "no changed source files to check".to_owned(),
                    ));
                    continue;
                }
                let Some(session) = current_session.as_mut() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };
                match check_changed_files_diagnostics(
                    &runtime,
                    agent.tool_registry(),
                    session,
                    &options,
                    root_config.code_intelligence.max_results,
                    changed_paths,
                ) {
                    Ok(result) => {
                        let _ = message_tx.send(WorkerMessage::Event(Box::new(
                            diagnostics_tool_event(result),
                        )));
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                    }
                }
            }
            Ok(WorkerCommand::PreviewCheckpointRestore {
                request_id,
                request,
            }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::CheckpointOperationFailed {
                        request_id,
                        error: "cannot preview checkpoint restore while the agent is running"
                            .to_owned(),
                    });
                    continue;
                }
                match preview_current_checkpoint_restore(
                    &current_session_log_path,
                    current_session.as_ref(),
                    &workspace_root,
                    &request,
                ) {
                    Ok(preview) => {
                        let _ = message_tx.send(WorkerMessage::CheckpointRestorePreviewed {
                            request_id,
                            preview,
                        });
                    }
                    Err(error) => {
                        let _ = message_tx
                            .send(WorkerMessage::CheckpointOperationFailed { request_id, error });
                    }
                }
            }
            Ok(WorkerCommand::ExecuteCheckpointRestore {
                request_id,
                request,
            }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::CheckpointOperationFailed {
                        request_id,
                        error: "cannot restore checkpoint while the agent is running".to_owned(),
                    });
                    continue;
                }
                let output = match execute_current_checkpoint_restore(
                    &current_session_log_path,
                    current_session.as_ref(),
                    &workspace_root,
                    &request,
                ) {
                    Ok(output) => output,
                    Err(error) => {
                        let _ = message_tx
                            .send(WorkerMessage::CheckpointOperationFailed { request_id, error });
                        continue;
                    }
                };
                match load_session_with_runtime_attachments(
                    &root_config.agent.provider,
                    &root_config.agent.model,
                    &current_session_log_path,
                    current_session.as_ref(),
                ) {
                    Ok(session) => {
                        let entries = session.entries().to_vec();
                        current_session = Some(session);
                        let _ = message_tx.send(WorkerMessage::CheckpointRestoreCompleted {
                            request_id,
                            preview: output.preview,
                            batch_id: output.batch_id,
                            entries,
                        });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::CheckpointOperationFailed {
                            request_id,
                            error: format!(
                                "checkpoint restored but session reload failed: {error:#}"
                            ),
                        });
                    }
                }
            }
            Ok(WorkerCommand::ForkConversationAtCheckpoint {
                request_id,
                request,
            }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::CheckpointOperationFailed {
                        request_id,
                        error: "cannot fork conversation while the agent is running".to_owned(),
                    });
                    continue;
                }
                let output = match fork_current_conversation(
                    &current_session_log_path,
                    current_session.as_ref(),
                    &request,
                ) {
                    Ok(output) => output,
                    Err(error) => {
                        let _ = message_tx
                            .send(WorkerMessage::CheckpointOperationFailed { request_id, error });
                        continue;
                    }
                };
                match load_session_with_runtime_attachments(
                    &root_config.agent.provider,
                    &root_config.agent.model,
                    &output.destination_path,
                    current_session.as_ref(),
                ) {
                    Ok(mut session) => {
                        if current_session.as_ref().is_some_and(|session| {
                            session_workspace_is_trusted(session, &workspace_root)
                        }) && let Err(error) = ensure_session_workspace_trust(
                            &mut session,
                            &workspace_root,
                            "trusted workspace carried into conversation fork",
                        ) {
                            let _ = message_tx.send(WorkerMessage::CheckpointOperationFailed {
                                request_id,
                                error,
                            });
                            continue;
                        }
                        exact_conversation_prompts.clear();
                        let entries = session.entries().to_vec();
                        let provider_name = session.provider_name().to_owned();
                        let model_name = session.model_name().to_owned();
                        current_session_log_path = output.destination_path.clone();
                        current_session = Some(session);
                        let _ = message_tx.send(WorkerMessage::ConversationForked {
                            request_id,
                            session_log_path: output.destination_path,
                            provider_name,
                            model_name,
                            copied_message_count: output.copied_message_count,
                            entries,
                        });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::CheckpointOperationFailed {
                            request_id,
                            error: format!(
                                "conversation fork created but session switch failed: {error:#}"
                            ),
                        });
                    }
                }
            }
            Ok(WorkerCommand::InspectLocalSession {
                request_id,
                source_path,
            }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                        request_id,
                        error: "cannot inspect session actions while the agent is running"
                            .to_owned(),
                    });
                    continue;
                }
                let service = local_session_lifecycle_service(&root_config, &workspace_root);
                match inspect_local_session(&service, &source_path) {
                    Ok(entry) => {
                        let _ = message_tx
                            .send(WorkerMessage::LocalSessionInspected { request_id, entry });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                            request_id,
                            error: format!("{error:#}"),
                        });
                    }
                }
            }
            Ok(WorkerCommand::ForkLocalSession {
                request_id,
                source_path,
            }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                        request_id,
                        error: "cannot fork a local session while the agent is running".to_owned(),
                    });
                    continue;
                }
                let service = local_session_lifecycle_service(&root_config, &workspace_root);
                let output = match fork_local_session(&service, &source_path) {
                    Ok(output) => output,
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                            request_id,
                            error: format!("{error:#}"),
                        });
                        continue;
                    }
                };
                match load_session_with_runtime_attachments(
                    &root_config.agent.provider,
                    &root_config.agent.model,
                    &output.destination_path,
                    current_session.as_ref(),
                ) {
                    Ok(mut session) => {
                        if current_session.as_ref().is_some_and(|session| {
                            session_workspace_is_trusted(session, &workspace_root)
                        }) && let Err(error) = ensure_session_workspace_trust(
                            &mut session,
                            &workspace_root,
                            "trusted workspace carried into local conversation fork",
                        ) {
                            let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                                request_id,
                                error,
                            });
                            continue;
                        }
                        pending_v2_compaction = None;
                        pending_queued_pre_turn_preparation = None;
                        compaction_preparation_tasks.abort_all();
                        exact_conversation_prompts.clear();
                        let entries = session.entries().to_vec();
                        let provider_name = session.provider_name().to_owned();
                        let model_name = session.model_name().to_owned();
                        current_session_log_path = output.destination_path.clone();
                        current_session = Some(session);
                        let _ = message_tx.send(WorkerMessage::LocalSessionForked {
                            request_id,
                            session_log_path: output.destination_path,
                            provider_name,
                            model_name,
                            copied_message_count: output.copied_message_count,
                            entries,
                        });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                            request_id,
                            error: format!(
                                "conversation fork created but session switch failed: {error:#}"
                            ),
                        });
                    }
                }
            }
            Ok(WorkerCommand::ExportLocalSession {
                request_id,
                source_path,
            }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                        request_id,
                        error: "cannot export a local session while the agent is running"
                            .to_owned(),
                    });
                    continue;
                }
                let service = local_session_lifecycle_service(&root_config, &workspace_root);
                match export_local_session(&service, &source_path) {
                    Ok(output) => {
                        let _ = message_tx
                            .send(WorkerMessage::LocalSessionExported { request_id, output });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                            request_id,
                            error: format!("{error:#}"),
                        });
                    }
                }
            }
            Ok(WorkerCommand::SetLocalSessionPin {
                request_id,
                source_path,
                pinned,
            }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                        request_id,
                        error: "cannot change a session pin while the agent is running".to_owned(),
                    });
                    continue;
                }
                let service = local_session_lifecycle_service(&root_config, &workspace_root);
                match set_local_session_pin(&service, &source_path, pinned) {
                    Ok(entry) => {
                        let _ = message_tx
                            .send(WorkerMessage::LocalSessionPinChanged { request_id, entry });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                            request_id,
                            error: format!("{error:#}"),
                        });
                    }
                }
            }
            Ok(WorkerCommand::PreviewLocalSessionDelete {
                request_id,
                source_path,
            }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                        request_id,
                        error: "cannot preview session deletion while the agent is running"
                            .to_owned(),
                    });
                    continue;
                }
                let service = local_session_lifecycle_service(&root_config, &workspace_root);
                match preview_local_session_delete(
                    &service,
                    &source_path,
                    std::slice::from_ref(&current_session_log_path),
                ) {
                    Ok(preview) => {
                        let _ = message_tx.send(WorkerMessage::LocalSessionDeletePreviewed {
                            request_id,
                            preview,
                        });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                            request_id,
                            error: format!("{error:#}"),
                        });
                    }
                }
            }
            Ok(WorkerCommand::ApplyLocalSessionDelete {
                request_id,
                preview,
            }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                        request_id,
                        error: "cannot delete a local session while the agent is running"
                            .to_owned(),
                    });
                    continue;
                }
                let service = local_session_lifecycle_service(&root_config, &workspace_root);
                match apply_local_session_delete(
                    &service,
                    &preview,
                    std::slice::from_ref(&current_session_log_path),
                ) {
                    Ok(output) => {
                        let _ = message_tx
                            .send(WorkerMessage::LocalSessionDeleted { request_id, output });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                            request_id,
                            error: format!("{error:#}"),
                        });
                    }
                }
            }
            Ok(WorkerCommand::PreviewSessionRetention { request_id, policy }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                        request_id,
                        error: "cannot preview session retention while the agent is running"
                            .to_owned(),
                    });
                    continue;
                }
                let service = local_session_lifecycle_service(&root_config, &workspace_root);
                match preview_session_retention(
                    &service,
                    policy,
                    std::slice::from_ref(&current_session_log_path),
                ) {
                    Ok(preview) => {
                        let _ = message_tx.send(WorkerMessage::SessionRetentionPreviewed {
                            request_id,
                            preview,
                        });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                            request_id,
                            error: format!("{error:#}"),
                        });
                    }
                }
            }
            Ok(WorkerCommand::ApplySessionRetention {
                request_id,
                preview,
            }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                        request_id,
                        error: "cannot apply session retention while the agent is running"
                            .to_owned(),
                    });
                    continue;
                }
                let service = local_session_lifecycle_service(&root_config, &workspace_root);
                match apply_session_retention(
                    &service,
                    &preview,
                    std::slice::from_ref(&current_session_log_path),
                ) {
                    Ok(output) => {
                        let _ = message_tx
                            .send(WorkerMessage::SessionRetentionApplied { request_id, output });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                            request_id,
                            error: format!("{error:#}"),
                        });
                    }
                }
            }
            Ok(WorkerCommand::CleanMutationArtifacts { target }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before cleaning mutation artifacts".to_owned(),
                    ));
                    continue;
                }
                match clean_mutation_artifacts(
                    &root_config,
                    &current_session_log_path,
                    &current_session,
                    &target,
                ) {
                    Ok(report) => {
                        let _ = message_tx.send(WorkerMessage::Notice(
                            format_mutation_artifact_cleanup_report(&report),
                        ));
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            Ok(WorkerCommand::DeleteMutationArtifact { artifact_id }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before deleting mutation artifacts".to_owned(),
                    ));
                    continue;
                }
                match delete_mutation_artifact(
                    &current_session_log_path,
                    &current_session,
                    &artifact_id,
                ) {
                    Ok(payload) => {
                        let _ = message_tx.send(WorkerMessage::Notice(
                            format_mutation_artifact_delete_report(&payload),
                        ));
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            Ok(WorkerCommand::ApproveVerificationCheck { check_spec_id }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before approving verification checks".to_owned(),
                    ));
                    continue;
                }
                match promote_workspace_verification_check(
                    &options.workspace_root,
                    &root_config,
                    &mut current_session,
                    &check_spec_id,
                    VerificationCheckPromotionKind::Approve,
                ) {
                    Ok(VerificationCheckPromotionOutcome::AlreadyPromoted { check_spec_id }) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "verification check already approved: {check_spec_id}"
                        )));
                    }
                    Ok(VerificationCheckPromotionOutcome::Promoted { entry }) => {
                        let check_spec_id = entry.trusted_check.check_spec.check_spec_id.clone();
                        let _ = message_tx.send(WorkerMessage::Event(Box::new(RunEvent::Control(
                            ControlEntry::CheckSpecRecorded(*entry),
                        ))));
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "verification check approved: {check_spec_id}"
                        )));
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            Ok(WorkerCommand::SandboxVerificationCheck { check_spec_id }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before sandboxing verification checks".to_owned(),
                    ));
                    continue;
                }
                match promote_workspace_verification_check(
                    &options.workspace_root,
                    &root_config,
                    &mut current_session,
                    &check_spec_id,
                    VerificationCheckPromotionKind::Sandbox,
                ) {
                    Ok(VerificationCheckPromotionOutcome::AlreadyPromoted { check_spec_id }) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "verification check already sandboxed: {check_spec_id}"
                        )));
                    }
                    Ok(VerificationCheckPromotionOutcome::Promoted { entry }) => {
                        let check_spec_id = entry.trusted_check.check_spec.check_spec_id.clone();
                        let _ = message_tx.send(WorkerMessage::Event(Box::new(RunEvent::Control(
                            ControlEntry::CheckSpecRecorded(*entry),
                        ))));
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "verification check sandboxed: {check_spec_id}"
                        )));
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            Ok(WorkerCommand::RerunTaskVerification { request }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before running verification".to_owned(),
                    ));
                    continue;
                }
                let Some(session) = current_session.as_mut() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "verification rerun requires an active session".to_owned(),
                    ));
                    continue;
                };
                let execution_backend =
                    match sigil_runtime::build_configured_execution_backend(&root_config) {
                        Ok(backend) => backend,
                        Err(error) => {
                            let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                                "failed to build verification execution backend: {error:#}"
                            )));
                            continue;
                        }
                    };
                let mut handler = ChannelEventHandler::new(message_tx.clone());
                match runtime.block_on(rerun_task_verification_check(
                    session,
                    &mut handler,
                    execution_backend.as_ref(),
                    &options.workspace_root,
                    &request,
                )) {
                    Ok(output) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "verification check {} {}",
                            output.check_run.check_spec_id,
                            match output.check_run.status {
                                sigil_kernel::VerificationCheckRunStatus::Succeeded => "passed",
                                sigil_kernel::VerificationCheckRunStatus::Failed => "failed",
                                sigil_kernel::VerificationCheckRunStatus::Skipped => "skipped",
                                sigil_kernel::VerificationCheckRunStatus::Inconclusive => {
                                    "inconclusive"
                                }
                                sigil_kernel::VerificationCheckRunStatus::Errored => "errored",
                                sigil_kernel::VerificationCheckRunStatus::Queued
                                | sigil_kernel::VerificationCheckRunStatus::Running => "finished",
                            }
                        )));
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                            "verification rerun failed: {error:#}"
                        )));
                    }
                }
            }
            Ok(WorkerCommand::RefreshProviderBalance {
                request_id,
                provider_config,
            }) => {
                provider_status_tasks.refresh_balance(
                    &runtime,
                    request_id,
                    provider_config,
                    provider_status_tx.clone(),
                );
            }
            Ok(WorkerCommand::RefreshProviderModels {
                request_id,
                provider_config,
            }) => {
                provider_status_tasks.refresh_models(
                    &runtime,
                    request_id,
                    provider_config,
                    provider_status_tx.clone(),
                );
            }
            Ok(WorkerCommand::CancelProviderModelsRefresh { request_id }) => {
                provider_status_tasks.cancel_models_refresh(request_id);
            }
            Ok(WorkerCommand::ActivateLazyMcp { server_name }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "cannot activate MCP while the agent is running".to_owned(),
                    ));
                    continue;
                }
                let Some(agent) = Arc::get_mut(&mut agent) else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "cannot activate MCP while agent registry is shared".to_owned(),
                    ));
                    continue;
                };
                let _ = message_tx.send(WorkerMessage::McpActivationStatus {
                    server_name: server_name.clone(),
                    status: McpActivationStatus::Activating,
                });
                let mutation_recorder = current_session
                    .as_ref()
                    .and_then(Session::mutation_event_recorder);
                match runtime.block_on(
                    sigil_runtime::activate_lazy_mcp_tools_detailed_with_mcp_handlers_and_mutation_recorder_and_network_admission(
                        agent.tool_registry_mut(),
                        &root_config,
                        &provider_capabilities,
                        options.workspace_root.clone(),
                        server_name.as_deref(),
                        elicitation_handler.clone(),
                        mcp_event_handler.clone(),
                        mutation_recorder,
                        sigil_kernel::ExtensionProcessNetworkAdmission::new(
                            options.permission_context.network_policy,
                            false,
                        ),
                    ),
                ) {
                    Ok(result) if result.matched_servers == 0 => {
                        let _ = message_tx.send(WorkerMessage::McpActivationStatus {
                            server_name: server_name.clone(),
                            status: McpActivationStatus::Deferred,
                        });
                        let detail = server_name
                            .as_deref()
                            .map(|name| format!(" for {name}"))
                            .unwrap_or_default();
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "no lazy MCP tools activated{detail}"
                        )));
                    }
                    Ok(result) => {
                        let _ = message_tx.send(WorkerMessage::McpActivationStatus {
                            server_name: server_name.clone(),
                            status: McpActivationStatus::Ready {
                                added_tools: result.added_tools,
                                process_coverage: sigil_runtime::mcp_process_receipts_summary(
                                    &result.process_launch_receipts,
                                ),
                            },
                        });
                        let detail = server_name
                            .as_deref()
                            .map(|name| format!(" for {name}"))
                            .unwrap_or_default();
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "activated {} lazy MCP tools{detail}",
                            result.added_tools
                        )));
                    }
                    Err(error) => {
                        let error = format!("{error:#}");
                        let _ = message_tx.send(WorkerMessage::McpActivationStatus {
                            server_name: server_name.clone(),
                            status: McpActivationStatus::Failed {
                                error: error.clone(),
                            },
                        });
                        let detail = server_name
                            .as_deref()
                            .map(|name| format!(" for {name}"))
                            .unwrap_or_default();
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "MCP activation failed{detail}: {error}"
                        )));
                    }
                }
            }
            Ok(WorkerCommand::RefreshMcpServer { server_name }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "cannot refresh MCP while the agent is running".to_owned(),
                    ));
                    continue;
                }
                pending_mcp_refreshes.insert(server_name);
                next_mcp_refresh_retry_at = Instant::now();
            }
            Ok(WorkerCommand::SwitchSession { session_log_path }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "cannot switch sessions while the agent is running".to_owned(),
                    ));
                    continue;
                }

                pending_v2_compaction = None;
                pending_queued_pre_turn_preparation = None;
                compaction_preparation_tasks.abort_all();

                match load_session_with_runtime_attachments(
                    &root_config.agent.provider,
                    &root_config.agent.model,
                    &session_log_path,
                    current_session.as_ref(),
                ) {
                    Ok(mut session) => {
                        let same_logical_session =
                            current_session.as_ref().is_some_and(|current| {
                                current.session_scope_id() == session.session_scope_id()
                            });
                        if !same_logical_session {
                            exact_conversation_prompts.clear();
                        }
                        mark_stale_dispatching_conversation_queue_items(
                            &mut session,
                            &exact_conversation_prompts,
                            &message_tx,
                        );
                        if current_session.as_ref().is_some_and(|session| {
                            session_workspace_is_trusted(session, &workspace_root)
                        }) {
                            match ensure_session_workspace_trust(
                                &mut session,
                                &workspace_root,
                                "trusted workspace carried into session",
                            ) {
                                Ok(()) => {}
                                Err(error) => {
                                    let _ = message_tx.send(WorkerMessage::RunFailed(error));
                                    continue;
                                }
                            }
                        }
                        let entries = session.entries().to_vec();
                        current_session_log_path = session_log_path.clone();
                        let provider_name = session.provider_name().to_owned();
                        let model_name = session.model_name().to_owned();
                        current_session = Some(session);
                        let _ = message_tx.send(WorkerMessage::SessionSwitched {
                            session_log_path,
                            provider_name,
                            model_name,
                            entries,
                        });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                    }
                }
            }
            Ok(WorkerCommand::StartNewSession { session_log_path }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "cannot start a new session while the agent is running".to_owned(),
                    ));
                    continue;
                }

                pending_v2_compaction = None;
                pending_queued_pre_turn_preparation = None;
                compaction_preparation_tasks.abort_all();

                match load_session_with_runtime_attachments(
                    &root_config.agent.provider,
                    &root_config.agent.model,
                    &session_log_path,
                    current_session.as_ref(),
                ) {
                    Ok(mut session) => {
                        let same_logical_session =
                            current_session.as_ref().is_some_and(|current| {
                                current.session_scope_id() == session.session_scope_id()
                            });
                        if !same_logical_session {
                            exact_conversation_prompts.clear();
                        }
                        mark_stale_dispatching_conversation_queue_items(
                            &mut session,
                            &exact_conversation_prompts,
                            &message_tx,
                        );
                        if current_session.as_ref().is_some_and(|session| {
                            session_workspace_is_trusted(session, &workspace_root)
                        }) {
                            match ensure_session_workspace_trust(
                                &mut session,
                                &workspace_root,
                                "trusted workspace carried into new session",
                            ) {
                                Ok(()) => {}
                                Err(error) => {
                                    let _ = message_tx.send(WorkerMessage::RunFailed(error));
                                    continue;
                                }
                            }
                        }
                        let entries = session.entries().to_vec();
                        current_session_log_path = session_log_path.clone();
                        let provider_name = session.provider_name().to_owned();
                        let model_name = session.model_name().to_owned();
                        current_session = Some(session);
                        let _ = message_tx.send(WorkerMessage::NewSessionStarted {
                            session_log_path,
                            provider_name,
                            model_name,
                            entries,
                        });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                    }
                }
            }
            Ok(WorkerCommand::Shutdown) => {
                if let Some(active_run) = active_run.take() {
                    cancel_active_run(
                        active_run,
                        &runtime,
                        &root_config,
                        &current_session_log_path,
                        &mut current_session,
                        &message_tx,
                        &elicitation_handler,
                        &agent_supervisor,
                        &mut discarded_run_ids,
                        "run interrupted by TUI shutdown",
                    );
                }
                provider_status_tasks.abort_all();
                compaction_preparation_tasks.abort_all();
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    provider_status_tasks.abort_all();
    compaction_preparation_tasks.abort_all();
}

fn finish_idle_auto_compaction(
    preparation: IdleAutoCompactionPreparation,
    current_session: &mut Option<Session>,
    current_session_log_path: &Path,
    message_tx: &mpsc::Sender<WorkerMessage>,
) {
    match preparation {
        IdleAutoCompactionPreparation::Ready(pending) => {
            let Some(session) = current_session.as_ref() else {
                return;
            };
            let provider_name = session.provider_name().to_owned();
            let model_name = session.model_name().to_owned();
            let folded_event_count = pending.folded_event_count();
            let idle_auto_scope_fingerprint =
                pending.idle_auto_scope_fingerprint().map(str::to_owned);
            match (*pending).apply(session, current_session_log_path) {
                Ok(outcome) => match load_session_with_runtime_attachments(
                    &provider_name,
                    &model_name,
                    current_session_log_path,
                    current_session.as_ref(),
                ) {
                    Ok(session) => {
                        let entries = session.entries().to_vec();
                        *current_session = Some(session);
                        let _ = message_tx.send(WorkerMessage::V2CompactionApplied {
                            request_id: 0,
                            source: V2CompactionApplySource::IdleAutomatic,
                            compaction_id: outcome.compaction_id,
                            folded_event_count,
                            entries,
                        });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "automatic compaction applied, but session reload was deferred: {error:#}"
                        )));
                    }
                },
                Err(error) => {
                    match load_session_with_runtime_attachments(
                        &provider_name,
                        &model_name,
                        current_session_log_path,
                        current_session.as_ref(),
                    ) {
                        Ok(session) => *current_session = Some(session),
                        Err(reload_error) => {
                            let _ = message_tx.send(WorkerMessage::Notice(format!(
                                "automatic compaction failed and session reload was deferred: {reload_error:#}"
                            )));
                        }
                    }
                    let latch_status = idle_auto_scope_fingerprint.as_deref().map_or_else(
                        || Ok(false),
                        |scope_fingerprint| {
                            has_failed_idle_automatic_scope(
                                current_session_log_path,
                                scope_fingerprint,
                            )
                        },
                    );
                    let notice = match latch_status {
                        Ok(true) => format!(
                            "automatic compaction was not applied; unchanged history is now held by its durable failure latch: {error:#}"
                        ),
                        Ok(false) => format!(
                            "automatic compaction was not applied before a durable failure latch could be confirmed: {error:#}"
                        ),
                        Err(latch_error) => format!(
                            "automatic compaction was not applied; durable failure latch status could not be confirmed ({latch_error:#}): {error:#}"
                        ),
                    };
                    let _ = message_tx.send(WorkerMessage::Notice(notice));
                }
            }
        }
        IdleAutoCompactionPreparation::NoFoldableHistory => {
            let _ = message_tx.send(WorkerMessage::Notice(
                "automatic compaction skipped: no newly foldable history".to_owned(),
            ));
        }
        IdleAutoCompactionPreparation::FailureLatched => {
            let _ = message_tx.send(WorkerMessage::Notice(
                "automatic compaction is held after a previous failed attempt; new fold material or target policy is required"
                    .to_owned(),
            ));
        }
        IdleAutoCompactionPreparation::CoolingDown {
            retry_after_unix_ms,
        } => {
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "automatic compaction admission is cooling down until {retry_after_unix_ms}"
            )));
        }
        IdleAutoCompactionPreparation::AdmissionUnavailable { reason } => {
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "automatic compaction was not applied: local target admission is unavailable ({reason})"
            )));
        }
        IdleAutoCompactionPreparation::NotRequested
        | IdleAutoCompactionPreparation::NotHardThreshold => {}
    }
}
