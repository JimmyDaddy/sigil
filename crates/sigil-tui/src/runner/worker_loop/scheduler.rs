use super::*;

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
    let initial_exact_conversation_prompts = ExactConversationPromptStore::new();
    let attachment_paths = sigil_runtime::resolve_sigil_paths(
        &root_config.storage,
        &root_config.session,
        &workspace_root,
    );
    let default_image_attachment_resolver: Arc<dyn ImageAttachmentResolver> = Arc::new(
        sigil_runtime::ControlledImageAttachmentCache::new(attachment_paths.attachments_root),
    );
    let initial_session = match load_session_with_runtime_attachments(
        &root_config.agent.provider,
        &root_config.agent.model,
        &session_log_path,
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
                &initial_exact_conversation_prompts,
                &message_tx,
            );
            Some(session)
        }
        Err(error) => {
            let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
            return;
        }
    };

    let session_entries = initial_session
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
    let mut state = WorkerLoopState::new(
        session_log_path,
        initial_session,
        agent_supervisor,
        background_agent_runs,
    );
    let _ = message_tx.send(WorkerMessage::WorkerReady);

    loop {
        if matches!(
            advance_worker_loop(WorkerAdvancementContext {
                runtime: &runtime,
                agent: &mut agent,
                root_config: &root_config,
                provider_capabilities: &provider_capabilities,
                workspace_root: &workspace_root,
                options: &options,
                message_tx: &message_tx,
                mcp_event_rx: &mcp_event_rx,
                elicitation_handler: &elicitation_handler,
                mcp_event_handler: &mcp_event_handler,
                context_resolver: &context_resolver,
                state: &mut state,
            }),
            WorkerAdvancementControl::SkipCommandPoll
        ) {
            continue;
        }

        let command = match command_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(command) => command,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if matches!(
            dispatch_worker_command(
                WorkerCommandContext {
                    runtime: &runtime,
                    agent: &mut agent,
                    root_config: &root_config,
                    provider_capabilities: &provider_capabilities,
                    workspace_root: &workspace_root,
                    options: &options,
                    message_tx: &message_tx,
                    elicitation_handler: &elicitation_handler,
                    mcp_event_handler: &mcp_event_handler,
                    role_provider_builder: &role_provider_builder,
                    context_resolver: &context_resolver,
                    state: &mut state,
                },
                command,
            ),
            WorkerCommandDispatchControl::Break
        ) {
            break;
        }
    }

    state.refresh.provider_status_tasks.abort_all();
    state.compaction.preparation_tasks.abort_all();
}

pub(in crate::runner) fn finish_idle_auto_compaction(
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
