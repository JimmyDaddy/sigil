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
        config_path: _,
        provider_capabilities: _,
        workspace_root,
        options,
        permission_mode_override: _,
        message_tx,
        elicitation_handler: _,
        mcp_event_handler: _,
        role_provider_builder: _,
        context_resolver,
        managed_extension_execution: _,
        managed_verification_execution: _,
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
                state.session.task_guidance_dirty = true;
                state.session.conversation_queue_dirty = true;
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
                state.session.task_guidance_dirty = true;
                state.session.conversation_queue_dirty = true;
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
                state.session.task_guidance_dirty = true;
                state.session.conversation_queue_dirty = true;
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
                state.session.task_guidance_dirty = true;
                state.session.conversation_queue_dirty = true;
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
                state.session.task_guidance_dirty = true;
                state.session.conversation_queue_dirty = true;
            }
            QueueCompactionCommand::SendQueuedConversationInputNow { queue_id } => {
                state.compaction.preparation_tasks.abort_all();
                state.session.pending_queued_pre_turn_preparation = None;
                // Non-destructive: the running turn is never cancelled. Promotion makes the
                // item the queue head so the kernel's safe-point injection (or the next
                // idle dispatch) delivers it without interrupting the current run.
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
                state.session.task_guidance_dirty = true;
                state.session.conversation_queue_dirty = true;
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
                if !paused {
                    state.session.task_guidance_dirty = true;
                    state.session.conversation_queue_dirty = true;
                }
            }
            QueueCompactionCommand::StartV2Compaction => {
                state.compaction.local_preview = None;
                state.compaction.pending = None;
                state.session.pending_queued_pre_turn_preparation = None;
                state.compaction.preparation_tasks.abort_all();
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "cannot compact context while the agent is running".to_owned(),
                    ));
                    continue;
                }
                if let Err(error) = state.acquire_route_execution_owner() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    continue;
                }
                let Some(session) = state.session.current.as_ref() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };
                let effective_config = options.compaction_config.clone();
                if !effective_config.enabled {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "compaction is disabled".to_owned(),
                    ));
                    continue;
                }
                match sigil_runtime::context_window::compaction_preview_for_strategy(
                    session,
                    &effective_config,
                ) {
                    Ok(Some(preview)) => {
                        let request_id = state.compaction.next_request_id;
                        state.compaction.next_request_id =
                            state.compaction.next_request_id.saturating_add(1);
                        let expected_session_scope_id = session.session_scope_id().to_owned();
                        let stable_snapshot = match capture_stable_idle_compaction_snapshot(session)
                        {
                            Ok(Some(snapshot)) => snapshot,
                            Ok(None) => {
                                let _ = message_tx.send(WorkerMessage::RunFailed(
                                    "compaction requires a stable active-session frontier"
                                        .to_owned(),
                                ));
                                continue;
                            }
                            Err(error) => {
                                let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                                    "failed to capture compaction source: {error:#}"
                                )));
                                continue;
                            }
                        };
                        let root_config = root_config.clone();
                        let workspace_root = workspace_root.clone();
                        let session_log_path = state.session.log_path.clone();
                        let options = options.clone();
                        let tools = agent.tool_registry().specs();
                        let runtime_handle = runtime.handle().clone();
                        let direct_context_resolver = context_resolver.clone();
                        let preparation_agent = std::sync::Arc::clone(agent);
                        let start_result = state.compaction.preparation_tasks.start_manual(
                            runtime,
                            request_id,
                            expected_session_scope_id.clone(),
                            std::sync::Arc::clone(&state.session.attachment_lease),
                            state.compaction.preparation_tx.clone(),
                            move || {
                                let Some(mut session) = stable_snapshot
                                    .materialize_compaction_session()
                                    .map_err(|error| format!("{error:#}"))?
                                else {
                                    return Err(
                                        "compaction source changed before preparation".to_owned()
                                    );
                                };
                                if session.session_scope_id() != expected_session_scope_id {
                                    return Err(
                                        "compaction preparation loaded a different session scope"
                                            .to_owned(),
                                    );
                                }
                                let (review, pending) = prepare_v2_compaction_summary_review(
                                    request_id,
                                    &root_config,
                                    &workspace_root,
                                    &session_log_path,
                                    preparation_agent.provider(),
                                    &mut session,
                                    &options,
                                    tools,
                                    &direct_context_resolver,
                                    &runtime_handle,
                                    preview,
                                )
                                .map_err(|error| format!("{error:#}"))?;
                                Ok(ManualV2CompactionPreparation {
                                    review,
                                    local_preview: None,
                                    pending,
                                    apply_source: V2CompactionApplySource::DirectCommand,
                                })
                            },
                        );
                        if let Err(error) = start_result {
                            let _ = message_tx.send(WorkerMessage::RunFailed(error));
                            continue;
                        }
                        let _ = message_tx.send(WorkerMessage::Notice(
                            "generating and validating one semantic compaction checkpoint"
                                .to_owned(),
                        ));
                    }
                    Ok(None) => {
                        let durable_message_count = session
                            .entries()
                            .iter()
                            .filter(|entry| {
                                matches!(
                                    entry,
                                    SessionLogEntry::User(_) | SessionLogEntry::Assistant(_)
                                )
                            })
                            .count();
                        let _ = message_tx.send(WorkerMessage::V2CompactionPreviewed {
                            state: V2CompactionPreviewState::NoFoldableHistory {
                                durable_message_count,
                                minimum_tail_turn_count:
                                    sigil_kernel::DEFAULT_TAIL_MIN_COMPLETE_TURNS,
                            },
                        });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                            "V2 compaction failed: {error:#}"
                        )));
                    }
                }
            }
            QueueCompactionCommand::PreviewV2Compaction => {
                state.compaction.local_preview = None;
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
                let effective_config = options.compaction_config.clone();
                if !effective_config.enabled {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "compaction is disabled".to_owned(),
                    ));
                    continue;
                }
                match sigil_runtime::context_window::compaction_preview_for_strategy(
                    session,
                    &effective_config,
                ) {
                    Ok(Some(preview)) => {
                        let request_id = state.compaction.next_request_id;
                        state.compaction.next_request_id =
                            state.compaction.next_request_id.saturating_add(1);
                        let expected_session_scope_id = session.session_scope_id().to_owned();
                        let stable_snapshot = match capture_stable_idle_compaction_snapshot(session)
                        {
                            Ok(Some(snapshot)) => snapshot,
                            Ok(None) => {
                                let _ = message_tx.send(WorkerMessage::RunFailed(
                                    "V2 compaction preview requires a stable active-session frontier"
                                        .to_owned(),
                                ));
                                continue;
                            }
                            Err(error) => {
                                let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                                    "failed to capture V2 compaction source: {error:#}"
                                )));
                                continue;
                            }
                        };
                        let root_config = root_config.clone();
                        let workspace_root = workspace_root.clone();
                        let session_log_path = state.session.log_path.clone();
                        let start_result = state.compaction.preparation_tasks.start_manual(
                            runtime,
                            request_id,
                            expected_session_scope_id.clone(),
                            std::sync::Arc::clone(&state.session.attachment_lease),
                            state.compaction.preparation_tx.clone(),
                            move || {
                                let Some(session) = stable_snapshot
                                    .materialize_compaction_session()
                                    .map_err(|error| format!("{error:#}"))?
                                else {
                                    return Err("V2 compaction source changed before preparation"
                                        .to_owned());
                                };
                                if session.session_scope_id() != expected_session_scope_id {
                                    return Err(
                                        "V2 compaction preparation loaded a different session scope"
                                            .to_owned(),
                                    );
                                }
                                let (review, local_preview) = prepare_v2_compaction_review(
                                    request_id,
                                    &root_config,
                                    &workspace_root,
                                    &session_log_path,
                                    &session,
                                    preview,
                                )
                                .map_err(|error| format!("{error:#}"))?;
                                Ok(ManualV2CompactionPreparation {
                                    review,
                                    local_preview: Some(local_preview),
                                    pending: None,
                                    apply_source: V2CompactionApplySource::ManualConfirmation,
                                })
                            },
                        );
                        if let Err(error) = start_result {
                            let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        }
                    }
                    Ok(None) => {
                        let durable_message_count = session
                            .entries()
                            .iter()
                            .filter(|entry| {
                                matches!(
                                    entry,
                                    SessionLogEntry::User(_) | SessionLogEntry::Assistant(_)
                                )
                            })
                            .count();
                        let _ = message_tx.send(WorkerMessage::V2CompactionPreviewed {
                            state: V2CompactionPreviewState::NoFoldableHistory {
                                durable_message_count,
                                minimum_tail_turn_count:
                                    sigil_kernel::DEFAULT_TAIL_MIN_COMPLETE_TURNS,
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
                if let Some(local_preview) = state.compaction.local_preview.take() {
                    if local_preview.request_id() != request_id {
                        let reviewed_request_id = local_preview.request_id();
                        state.compaction.local_preview = Some(local_preview);
                        let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                            request_id,
                            error: format!(
                                "stale local compaction confirmation (review request is {reviewed_request_id})"
                            ),
                        });
                        continue;
                    }
                    let Some(session) = state.session.current.as_ref() else {
                        state.compaction.local_preview = Some(local_preview);
                        let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                            request_id,
                            error: "session state is unavailable".to_owned(),
                        });
                        continue;
                    };
                    if session.session_scope_id() != local_preview.session_scope_id() {
                        let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                            request_id,
                            error: "local compaction review belongs to a different session scope"
                                .to_owned(),
                        });
                        continue;
                    }
                    let expected_session_scope_id = session.session_scope_id().to_owned();
                    let stable_snapshot = match capture_stable_idle_compaction_snapshot(session) {
                        Ok(Some(snapshot)) => snapshot,
                        Ok(None) => {
                            state.compaction.local_preview = Some(local_preview);
                            let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                                request_id,
                                error:
                                    "semantic compaction requires a stable active-session frontier"
                                        .to_owned(),
                            });
                            continue;
                        }
                        Err(error) => {
                            state.compaction.local_preview = Some(local_preview);
                            let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                                request_id,
                                error: format!(
                                    "failed to capture semantic compaction source: {error:#}"
                                ),
                            });
                            continue;
                        }
                    };
                    let root_config = root_config.clone();
                    let workspace_root = workspace_root.clone();
                    let session_log_path = state.session.log_path.clone();
                    let options = options.clone();
                    let tools = agent.tool_registry().specs();
                    let runtime_handle = runtime.handle().clone();
                    let manual_context_resolver = context_resolver.clone();
                    let preparation_agent = std::sync::Arc::clone(agent);
                    let preview = local_preview.preview().clone();
                    let start_result = state.compaction.preparation_tasks.start_manual(
                        runtime,
                        request_id,
                        expected_session_scope_id.clone(),
                        std::sync::Arc::clone(&state.session.attachment_lease),
                        state.compaction.preparation_tx.clone(),
                        move || {
                            let Some(mut session) = stable_snapshot
                                .materialize_compaction_session()
                                .map_err(|error| format!("{error:#}"))?
                            else {
                                return Err(
                                    "semantic compaction source changed before preparation"
                                        .to_owned(),
                                );
                            };
                            if session.session_scope_id() != expected_session_scope_id {
                                return Err(
                                    "semantic compaction preparation loaded a different session scope"
                                        .to_owned(),
                                );
                            }
                            let (review, pending) = prepare_v2_compaction_summary_review(
                                request_id,
                                &root_config,
                                &workspace_root,
                                &session_log_path,
                                preparation_agent.provider(),
                                &mut session,
                                &options,
                                tools,
                                &manual_context_resolver,
                                &runtime_handle,
                                preview,
                            )
                            .map_err(|error| format!("{error:#}"))?;
                            Ok(ManualV2CompactionPreparation {
                                review,
                                local_preview: None,
                                pending,
                                apply_source: V2CompactionApplySource::ManualConfirmation,
                            })
                        },
                    );
                    if let Err(error) = start_result {
                        let _ = message_tx
                            .send(WorkerMessage::V2CompactionApplyFailed { request_id, error });
                        continue;
                    }
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "generating one billed semantic compaction summary".to_owned(),
                    ));
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
                if let Err(error) = state.acquire_route_execution_owner() {
                    state.compaction.pending = Some(pending);
                    let _ = message_tx
                        .send(WorkerMessage::V2CompactionApplyFailed { request_id, error });
                    continue;
                }
                let Some(session) = state.session.current.as_ref() else {
                    state.compaction.pending = Some(pending);
                    let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                        request_id,
                        error: "session state is unavailable".to_owned(),
                    });
                    continue;
                };
                let folded_event_count = pending.folded_event_count();
                let applied = pending.apply_with_optional_native(
                    session,
                    &state.session.log_path,
                    agent.provider(),
                    runtime,
                    root_config.compaction.native_carrier_enabled,
                );
                match applied {
                    Ok((outcome, native_notice)) => {
                        if let Some(notice) = native_notice {
                            let _ = message_tx.send(WorkerMessage::Notice(notice));
                        }
                        let entries = state
                            .session
                            .current
                            .as_ref()
                            .map(|current| current.entries().to_vec())
                            .unwrap_or_default();
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
            QueueCompactionCommand::ApplyStandaloneToolOutputShrink { request_id } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                        request_id,
                        error: "cannot clean tool outputs while the agent is running".to_owned(),
                    });
                    continue;
                }
                let Some(local_preview) = state.compaction.local_preview.take() else {
                    let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                        request_id,
                        error: "no local compaction review is pending".to_owned(),
                    });
                    continue;
                };
                if local_preview.request_id() != request_id {
                    let reviewed_request_id = local_preview.request_id();
                    state.compaction.local_preview = Some(local_preview);
                    let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                        request_id,
                        error: format!(
                            "stale standalone shrink confirmation (review request is {reviewed_request_id})"
                        ),
                    });
                    continue;
                }
                let Some(session) = state.session.current.as_ref() else {
                    state.compaction.local_preview = Some(local_preview);
                    let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                        request_id,
                        error: "session state is unavailable".to_owned(),
                    });
                    continue;
                };
                if session.session_scope_id() != local_preview.session_scope_id() {
                    let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                        request_id,
                        error: "standalone shrink review belongs to a different session scope"
                            .to_owned(),
                    });
                    continue;
                }
                let active = match session.active_projection_snapshot() {
                    Ok(Some(active)) => active,
                    Ok(None) => {
                        state.compaction.local_preview = Some(local_preview);
                        let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                            request_id,
                            error: "standalone cleanup requires a durable active projection"
                                .to_owned(),
                        });
                        continue;
                    }
                    Err(error) => {
                        state.compaction.local_preview = Some(local_preview);
                        let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                            request_id,
                            error: format!("failed to read tool-output pressure: {error:#}"),
                        });
                        continue;
                    }
                };
                let pressure = active.tool_output_pressure();
                let batch = match sigil_kernel::ToolOutputAgingBatchV1::select(
                    &pressure,
                    sigil_kernel::ToolOutputAgingReasonV1::Manual,
                ) {
                    Ok(Some(batch)) => batch,
                    Ok(None) => {
                        let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                            request_id,
                            error: "no new large historical tool outputs are eligible".to_owned(),
                        });
                        continue;
                    }
                    Err(error) => {
                        state.compaction.local_preview = Some(local_preview);
                        let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                            request_id,
                            error: format!("failed to select tool-output cleanup: {error:#}"),
                        });
                        continue;
                    }
                };
                let activation =
                    match sigil_kernel::ToolOutputAgingActivatedV1::prepare(&pressure, &batch) {
                        Ok(activation) => activation,
                        Err(error) => {
                            state.compaction.local_preview = Some(local_preview);
                            let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                                request_id,
                                error: format!("failed to prepare tool-output cleanup: {error:#}"),
                            });
                            continue;
                        }
                    };
                let projected_output_count = activation.replacements.len();
                let context_epoch_id = activation.target_epoch_id.clone();
                match session.append_tool_output_aging_activation(active.frontier(), activation) {
                    Ok(Some(_)) => {
                        let entries = state
                            .session
                            .current
                            .as_ref()
                            .map(|current| current.entries().to_vec())
                            .unwrap_or_default();
                        let _ = message_tx.send(WorkerMessage::StandaloneToolOutputShrinkApplied {
                            request_id,
                            context_epoch_id,
                            projected_output_count,
                            entries,
                        });
                    }
                    Ok(None) => {
                        let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                            request_id,
                            error: "no standalone tool-output projection was appended".to_owned(),
                        });
                    }
                    Err(error) => {
                        state.compaction.local_preview = Some(local_preview);
                        let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                            request_id,
                            error: format!("standalone tool-output cleanup failed: {error:#}"),
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
                } else if state
                    .compaction
                    .local_preview
                    .as_ref()
                    .is_some_and(|pending| pending.request_id() == request_id)
                {
                    state.compaction.local_preview = None;
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "discarded local compaction review without provider consumption".to_owned(),
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
