use super::*;

pub(super) fn dispatch_intent_stack_command<P>(
    context: WorkerCommandContext<'_, P>,
    command: IntentStackCommand,
) -> WorkerCommandDispatchControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerCommandContext {
        runtime: _,
        agent: _,
        root_config,
        config_path: _,
        provider_capabilities: _,
        workspace_root,
        options: _,
        permission_mode_override: _,
        message_tx,
        elicitation_handler: _,
        mcp_event_handler: _,
        role_provider_builder: _,
        context_resolver: _,
        managed_extension_execution: _,
        managed_verification_execution: _,
        state,
    } = context;
    match command {
        IntentStackCommand::Load { request_id } => {
            let result = state
                .session
                .current
                .as_ref()
                .ok_or_else(|| "session state is unavailable".to_owned())
                .and_then(|session| {
                    sigil_runtime::execute_application_intent_stack_command(
                        session,
                        root_config,
                        workspace_root,
                        &sigil_runtime::ApplicationIntentStackCommandV1::Inspect,
                        sigil_runtime::ApplicationIntentConfirmationSource::Tui,
                    )
                    .map_err(|error| format!("{error:#}"))
                    .and_then(|output| match output {
                        sigil_runtime::ApplicationIntentStackCommandOutputV1::Projection {
                            state,
                        } => Ok(state),
                        _ => Err("Intent Stack inspect returned an invalid result".to_owned()),
                    })
                });
            match result {
                Ok(stack_state) => {
                    let _ = message_tx.send(WorkerMessage::IntentStackLoaded {
                        request_id,
                        stack_state,
                    });
                }
                Err(error) => {
                    send_intent_failure(message_tx, request_id, error);
                }
            }
        }
        IntentStackCommand::PreviewDrop {
            request_id,
            intent_ref,
        } => {
            if state.run.active.is_some() {
                send_intent_failure(
                    message_tx,
                    request_id,
                    "wait for the active run before previewing Intent drop".to_owned(),
                );
                return WorkerCommandDispatchControl::Continue;
            }
            let result = state
                .session
                .current
                .as_ref()
                .ok_or_else(|| "session state is unavailable".to_owned())
                .and_then(|session| {
                    sigil_runtime::execute_application_intent_stack_command(
                        session,
                        root_config,
                        workspace_root,
                        &sigil_runtime::ApplicationIntentStackCommandV1::PreviewDrop { intent_ref },
                        sigil_runtime::ApplicationIntentConfirmationSource::Tui,
                    )
                    .map_err(|error| format!("{error:#}"))
                    .and_then(|output| match output {
                        sigil_runtime::ApplicationIntentStackCommandOutputV1::DropPreview {
                            preview,
                        } => Ok(preview),
                        _ => Err("Intent drop preview returned an invalid result".to_owned()),
                    })
                });
            match result {
                Ok(preview) => {
                    let _ = message_tx.send(WorkerMessage::IntentDropPreviewed {
                        request_id,
                        preview,
                    });
                }
                Err(error) => {
                    send_intent_failure(message_tx, request_id, error);
                }
            }
        }
        IntentStackCommand::ExecuteDrop {
            request_id,
            request,
        } => {
            if state.run.active.is_some() {
                send_intent_failure(
                    message_tx,
                    request_id,
                    "wait for the active run before dropping an Intent".to_owned(),
                );
                return WorkerCommandDispatchControl::Continue;
            }
            let Some(session) = state.session.current.as_ref() else {
                send_intent_failure(
                    message_tx,
                    request_id,
                    "session state is unavailable".to_owned(),
                );
                return WorkerCommandDispatchControl::Continue;
            };
            let execution = match sigil_runtime::execute_application_intent_stack_command(
                session,
                root_config,
                workspace_root,
                &sigil_runtime::ApplicationIntentStackCommandV1::ExecuteDrop { request },
                sigil_runtime::ApplicationIntentConfirmationSource::Tui,
            ) {
                Ok(sigil_runtime::ApplicationIntentStackCommandOutputV1::DropExecution {
                    execution,
                }) => execution,
                Ok(_) => {
                    send_intent_failure(
                        message_tx,
                        request_id,
                        "Intent drop returned an invalid result".to_owned(),
                    );
                    return WorkerCommandDispatchControl::Continue;
                }
                Err(error) => {
                    send_intent_failure(message_tx, request_id, format!("{error:#}"));
                    return WorkerCommandDispatchControl::Continue;
                }
            };
            let provider_name = session.provider_name().to_owned();
            let model_name = session.model_name().to_owned();
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
                        "Intent drop resolved, but session reload was deferred: {error:#}"
                    )));
                    state
                        .session
                        .current
                        .as_ref()
                        .map(|current| current.entries().to_vec())
                        .unwrap_or_default()
                }
            };
            let stack_state = match state
                .session
                .current
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("session state is unavailable after Intent drop"))
                .and_then(|session| session.public_intent_stack_state_for_workspace(workspace_root))
            {
                Ok(stack_state) => stack_state,
                Err(error) => {
                    send_intent_failure(
                        message_tx,
                        request_id,
                        format!("Intent drop resolved but projection reload failed: {error:#}"),
                    );
                    return WorkerCommandDispatchControl::Continue;
                }
            };
            let _ = message_tx.send(WorkerMessage::IntentDropCompleted {
                request_id,
                execution,
                stack_state,
                entries,
            });
        }
    }
    WorkerCommandDispatchControl::Continue
}

fn send_intent_failure(message_tx: &mpsc::Sender<WorkerMessage>, request_id: u64, error: String) {
    let _ = message_tx.send(WorkerMessage::IntentStackOperationFailed { request_id, error });
}
