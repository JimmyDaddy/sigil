use super::*;
use crate::runner::V2CompactionPreviewState;

pub(super) fn dispatch_queue_compaction_command<P>(
    context: WorkerCommandContext<'_, P>,
    command: QueueCompactionCommand,
) -> WorkerCommandDispatchControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerCommandContext {
        runtime,
        agent,
        root_config,
        provider_capabilities: _,
        workspace_root,
        options,
        message_tx,
        elicitation_handler,
        mcp_event_handler: _,
        role_provider_builder: _,
        context_resolver,
        state,
    } = context;
    let mut command_result = Some(command);
    let control = WorkerCommandDispatchControl::Continue;
    while let Some(command_result) = command_result.take() {
        match command_result {
            QueueCompactionCommand::QueueConversationInput {
                prompt,
                kind,
                target,
                reasoning_effort,
            } => {
                state.compaction.preparation_tasks.abort_all();
                state.session.pending_queued_pre_turn_preparation = None;
                match queue_conversation_input_and_track_detached(
                    &state.session.log_path,
                    &mut state.session.current,
                    &mut state.session.detached_durable_controls,
                    &mut state.session.exact_prompts,
                    prompt,
                    kind,
                    target,
                    reasoning_effort,
                ) {
                    Ok(entries) => {
                        send_conversation_queue_update(message_tx, &entries);
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            QueueCompactionCommand::CancelQueuedConversationInput { queue_id } => {
                state.compaction.preparation_tasks.abort_all();
                state.session.pending_queued_pre_turn_preparation = None;
                match cancel_queued_conversation_input(
                    &state.session.log_path,
                    &mut state.session.current,
                    &mut state.session.detached_durable_controls,
                    &mut state.session.exact_prompts,
                    queue_id,
                ) {
                    Ok(entries) => send_conversation_queue_update(message_tx, &entries),
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            QueueCompactionCommand::EditQueuedConversationInput {
                queue_id,
                prompt,
                reasoning_effort,
            } => {
                state.compaction.preparation_tasks.abort_all();
                state.session.pending_queued_pre_turn_preparation = None;
                match edit_queued_conversation_input(
                    &state.session.log_path,
                    &mut state.session.current,
                    &mut state.session.detached_durable_controls,
                    &mut state.session.exact_prompts,
                    queue_id,
                    prompt,
                    reasoning_effort,
                ) {
                    Ok(entries) => send_conversation_queue_update(message_tx, &entries),
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            QueueCompactionCommand::MoveQueuedConversationInput {
                queue_id,
                direction,
            } => {
                state.compaction.preparation_tasks.abort_all();
                state.session.pending_queued_pre_turn_preparation = None;
                match move_queued_conversation_input(
                    &state.session.log_path,
                    &mut state.session.current,
                    &mut state.session.detached_durable_controls,
                    queue_id,
                    direction,
                ) {
                    Ok(entries) => send_conversation_queue_update(message_tx, &entries),
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            QueueCompactionCommand::PromoteQueuedConversationInput { queue_id } => {
                state.compaction.preparation_tasks.abort_all();
                state.session.pending_queued_pre_turn_preparation = None;
                match promote_queued_conversation_input(
                    &state.session.log_path,
                    &mut state.session.current,
                    &mut state.session.detached_durable_controls,
                    queue_id,
                ) {
                    Ok(entries) => send_conversation_queue_update(message_tx, &entries),
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            QueueCompactionCommand::SendQueuedConversationInputNow { queue_id } => {
                state.compaction.preparation_tasks.abort_all();
                state.session.pending_queued_pre_turn_preparation = None;
                match promote_queued_conversation_input(
                    &state.session.log_path,
                    &mut state.session.current,
                    &mut state.session.detached_durable_controls,
                    queue_id,
                ) {
                    Ok(entries) => {
                        send_conversation_queue_update(message_tx, &entries);
                        if let Some(run) = state.run.active.take() {
                            cancel_active_run(
                                run,
                                runtime,
                                root_config,
                                &state.session.log_path,
                                &mut state.session.current,
                                &mut state.session.detached_durable_controls,
                                message_tx,
                                elicitation_handler,
                                &state.agent.supervisor,
                                &mut state.run.discarded_ids,
                                "run interrupted for follow-up",
                            );
                        }
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            QueueCompactionCommand::SetConversationQueuePaused { paused } => {
                match set_conversation_queue_paused(
                    &state.session.log_path,
                    &mut state.session.current,
                    &mut state.session.detached_durable_controls,
                    paused,
                ) {
                    Ok(entries) => send_conversation_queue_update(message_tx, &entries),
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            QueueCompactionCommand::PreviewV2Compaction => {
                state.compaction.pending = None;
                state.session.pending_queued_pre_turn_preparation = None;
                state.compaction.preparation_tasks.abort_all();
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "cannot preview compaction while the agent is running".to_owned(),
                    ));
                    continue;
                }
                let Some(session) = state.session.current.as_ref() else {
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
                        let manual_context_resolver = context_resolver.clone();
                        let runtime_attachments =
                            CapturedSessionRuntimeAttachments::from_session(Some(session));
                        state.compaction.preparation_tasks.start_manual(
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
            QueueCompactionCommand::ApplyV2Compaction { request_id } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                        request_id,
                        error: "cannot apply compaction while the agent is running".to_owned(),
                    });
                    continue;
                }
                let Some(pending) = state.compaction.pending.take() else {
                    let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                        request_id,
                        error: "no admitted V2 compaction review is pending".to_owned(),
                    });
                    continue;
                };
                if pending.request_id() != request_id {
                    let reviewed_request_id = pending.request_id();
                    state.compaction.pending = Some(pending);
                    let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                        request_id,
                        error: format!(
                            "stale V2 compaction confirmation (review request is {reviewed_request_id})"
                        ),
                    });
                    continue;
                }
                let Some(session) = state.session.current.as_ref() else {
                    let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                        request_id,
                        error: "session state is unavailable".to_owned(),
                    });
                    continue;
                };
                let provider_name = session.provider_name().to_owned();
                let model_name = session.model_name().to_owned();
                let folded_event_count = pending.folded_event_count();
                let applied = pending.apply(session, &state.session.log_path);
                match applied {
                    Ok(outcome) => {
                        let reloaded = load_session_with_runtime_attachments(
                            &provider_name,
                            &model_name,
                            &state.session.log_path,
                            state.session.current.as_ref(),
                        );
                        let entries = match reloaded {
                            Ok(session) => {
                                let entries = session.entries().to_vec();
                                state.session.current = Some(session);
                                entries
                            }
                            Err(error) => {
                                let _ = message_tx.send(WorkerMessage::Notice(format!(
                                    "compaction applied, but session reload was deferred: {error:#}"
                                )));
                                state
                                    .session
                                    .current
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
            QueueCompactionCommand::CancelV2CompactionReview { request_id } => {
                let preparation_cancelled = state.compaction.preparation_tasks.cancel(request_id);
                if state
                    .compaction
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.request_id() == request_id)
                {
                    state.compaction.pending = None;
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "discarded pending V2 compaction review".to_owned(),
                    ));
                } else if preparation_cancelled {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "cancelled V2 compaction preparation".to_owned(),
                    ));
                }
            }
        }
    }
    control
}
