use super::*;

pub(super) fn dispatch_session_command<P>(
    context: WorkerCommandContext<'_, P>,
    command: SessionCommand,
) -> WorkerCommandDispatchControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerCommandContext {
        runtime: _,
        agent,
        root_config,
        provider_capabilities,
        workspace_root,
        options: _,
        message_tx,
        elicitation_handler: _,
        mcp_event_handler: _,
        role_provider_builder: _,
        context_resolver: _,
        state,
    } = context;
    let mut command_result = Some(command);
    let control = WorkerCommandDispatchControl::Continue;
    while let Some(command_result) = command_result.take() {
        match command_result {
            SessionCommand::InspectLocalSession {
                request_id,
                source_path,
            } => {
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
            SessionCommand::ForkLocalSession {
                request_id,
                source_path,
            } => {
                if let Err(error) =
                    ensure_session_transition_allowed(SessionTransitionKind::LocalFork, state)
                {
                    let _ = message_tx
                        .send(WorkerMessage::LocalSessionLifecycleFailed { request_id, error });
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
                match transition_session(
                    SessionTransitionKind::LocalFork,
                    output.destination_path.clone(),
                    root_config,
                    provider_capabilities,
                    workspace_root,
                    agent,
                    state,
                    message_tx,
                ) {
                    Ok(transition) => {
                        let _ = message_tx.send(WorkerMessage::LocalSessionForked {
                            request_id,
                            session_log_path: transition.session_log_path,
                            provider_name: transition.provider_name,
                            model_name: transition.model_name,
                            copied_message_count: output.copied_message_count,
                            entries: transition.entries,
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
            SessionCommand::ExportLocalSession {
                request_id,
                source_path,
            } => {
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
            SessionCommand::SetLocalSessionPin {
                request_id,
                source_path,
                pinned,
            } => {
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
            SessionCommand::PreviewLocalSessionDelete {
                request_id,
                source_path,
            } => {
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
            SessionCommand::ApplyLocalSessionDelete {
                request_id,
                preview,
            } => {
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
            SessionCommand::PreviewSessionRetention { request_id, policy } => {
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
            SessionCommand::ApplySessionRetention {
                request_id,
                preview,
            } => {
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
            SessionCommand::SwitchSession { session_log_path } => {
                match transition_session(
                    SessionTransitionKind::Switch,
                    session_log_path,
                    root_config,
                    provider_capabilities,
                    workspace_root,
                    agent,
                    state,
                    message_tx,
                ) {
                    Ok(transition) => {
                        let _ = message_tx.send(WorkerMessage::SessionSwitched {
                            session_log_path: transition.session_log_path,
                            provider_name: transition.provider_name,
                            model_name: transition.model_name,
                            entries: transition.entries,
                        });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            SessionCommand::StartNewSession { session_log_path } => {
                match transition_session(
                    SessionTransitionKind::StartNew,
                    session_log_path,
                    root_config,
                    provider_capabilities,
                    workspace_root,
                    agent,
                    state,
                    message_tx,
                ) {
                    Ok(transition) => {
                        let _ = message_tx.send(WorkerMessage::NewSessionStarted {
                            session_log_path: transition.session_log_path,
                            provider_name: transition.provider_name,
                            model_name: transition.model_name,
                            entries: transition.entries,
                        });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
        }
    }
    control
}
