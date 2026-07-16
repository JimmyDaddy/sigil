use super::*;

pub(super) fn dispatch_session_command<P>(
    context: WorkerCommandContext<'_, P>,
    command: WorkerCommand,
) -> WorkerCommandDispatchControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerCommandContext {
        runtime: _,
        agent: _,
        root_config,
        provider_capabilities: _,
        workspace_root,
        options: _,
        message_tx,
        elicitation_handler: _,
        mcp_event_handler: _,
        role_provider_builder: _,
        context_resolver: _,
        state,
    } = context;
    let mut command_result: Option<Result<WorkerCommand, mpsc::RecvTimeoutError>> =
        Some(Ok(command));
    let control = WorkerCommandDispatchControl::Continue;
    while let Some(command_result) = command_result.take() {
        match command_result {
            Ok(WorkerCommand::InspectLocalSession {
                request_id,
                source_path,
            }) => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                        request_id,
                        error: "cannot inspect session actions while the agent is running"
                            .to_owned(),
                    });
                    continue;
                }
                let service = local_session_lifecycle_service(root_config, workspace_root);
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
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                        request_id,
                        error: "cannot fork a local session while the agent is running".to_owned(),
                    });
                    continue;
                }
                let service = local_session_lifecycle_service(root_config, workspace_root);
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
                    state.session.current.as_ref(),
                ) {
                    Ok(mut session) => {
                        if state.session.current.as_ref().is_some_and(|session| {
                            session_workspace_is_trusted(session, workspace_root)
                        }) && let Err(error) = ensure_session_workspace_trust(
                            &mut session,
                            workspace_root,
                            "trusted workspace carried into local conversation fork",
                        ) {
                            let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                                request_id,
                                error,
                            });
                            continue;
                        }
                        state.compaction.pending = None;
                        state.session.pending_queued_pre_turn_preparation = None;
                        state.compaction.preparation_tasks.abort_all();
                        state.session.exact_prompts.clear();
                        let entries = session.entries().to_vec();
                        let provider_name = session.provider_name().to_owned();
                        let model_name = session.model_name().to_owned();
                        state.session.log_path = output.destination_path.clone();
                        state.session.current = Some(session);
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
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                        request_id,
                        error: "cannot export a local session while the agent is running"
                            .to_owned(),
                    });
                    continue;
                }
                let service = local_session_lifecycle_service(root_config, workspace_root);
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
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                        request_id,
                        error: "cannot change a session pin while the agent is running".to_owned(),
                    });
                    continue;
                }
                let service = local_session_lifecycle_service(root_config, workspace_root);
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
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                        request_id,
                        error: "cannot preview session deletion while the agent is running"
                            .to_owned(),
                    });
                    continue;
                }
                let service = local_session_lifecycle_service(root_config, workspace_root);
                match preview_local_session_delete(
                    &service,
                    &source_path,
                    std::slice::from_ref(&state.session.log_path),
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
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                        request_id,
                        error: "cannot delete a local session while the agent is running"
                            .to_owned(),
                    });
                    continue;
                }
                let service = local_session_lifecycle_service(root_config, workspace_root);
                match apply_local_session_delete(
                    &service,
                    &preview,
                    std::slice::from_ref(&state.session.log_path),
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
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                        request_id,
                        error: "cannot preview session retention while the agent is running"
                            .to_owned(),
                    });
                    continue;
                }
                let service = local_session_lifecycle_service(root_config, workspace_root);
                match preview_session_retention(
                    &service,
                    policy,
                    std::slice::from_ref(&state.session.log_path),
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
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                        request_id,
                        error: "cannot apply session retention while the agent is running"
                            .to_owned(),
                    });
                    continue;
                }
                let service = local_session_lifecycle_service(root_config, workspace_root);
                match apply_session_retention(
                    &service,
                    &preview,
                    std::slice::from_ref(&state.session.log_path),
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
            Ok(WorkerCommand::SwitchSession { session_log_path }) => {
                match transition_session(
                    SessionTransitionKind::Switch,
                    session_log_path,
                    root_config,
                    workspace_root,
                    state,
                    message_tx,
                ) {
                    Ok(message) => {
                        let _ = message_tx.send(message);
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            Ok(WorkerCommand::StartNewSession { session_log_path }) => {
                match transition_session(
                    SessionTransitionKind::StartNew,
                    session_log_path,
                    root_config,
                    workspace_root,
                    state,
                    message_tx,
                ) {
                    Ok(message) => {
                        let _ = message_tx.send(message);
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            Ok(command) => unreachable!(
                "exhaustive classifier routed an unexpected command to session: {command:?}"
            ),
            Err(error) => unreachable!("owned command dispatch received channel error: {error}"),
        }
    }
    control
}
