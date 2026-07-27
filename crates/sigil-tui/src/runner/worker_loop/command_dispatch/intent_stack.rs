use super::*;

const INTENT_DROP_CONFIRMATION_TTL_MS: u64 = 5 * 60 * 1_000;

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
    match command {
        IntentStackCommand::Load { request_id } => {
            let result = state
                .session
                .current
                .as_ref()
                .ok_or_else(|| "session state is unavailable".to_owned())
                .and_then(|session| {
                    session
                        .public_intent_stack_state_for_workspace(workspace_root)
                        .map_err(|error| format!("{error:#}"))
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
                    sigil_kernel::preview_intent_drop(session, workspace_root, &intent_ref)
                        .map_err(|error| format!("{error:#}"))
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
            if root_config.permission.mode == sigil_kernel::PermissionMode::ReadOnly {
                send_intent_failure(
                    message_tx,
                    request_id,
                    "read-only permission mode denies Intent drop".to_owned(),
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
            if !session_workspace_is_trusted(session, workspace_root) {
                send_intent_failure(
                    message_tx,
                    request_id,
                    "workspace trust is required before Intent drop".to_owned(),
                );
                return WorkerCommandDispatchControl::Continue;
            }
            let authority = match intent_drop_authority(root_config, &request) {
                Ok(authority) => authority,
                Err(error) => {
                    send_intent_failure(message_tx, request_id, error);
                    return WorkerCommandDispatchControl::Continue;
                }
            };
            let execution = match sigil_kernel::execute_intent_drop(
                session,
                workspace_root,
                &request,
                &authority,
                "user confirmed exact TUI Intent drop preview",
            ) {
                Ok(execution) => execution,
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

fn intent_drop_authority(
    root_config: &RootConfig,
    request: &sigil_kernel::IntentDropRequestV1,
) -> std::result::Result<sigil_kernel::IntentOperationAuthorityV1, String> {
    let permission_json =
        serde_json::to_vec(&root_config.permission).map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    hasher.update(b"sigil.intent.permission_policy.v1\0");
    hasher.update(permission_json);
    let digest = sigil_kernel::IntentDigest::new(format!(
        "{}{:x}",
        sigil_kernel::INTENT_CANONICAL_DIGEST_PREFIX,
        hasher.finalize()
    ))
    .map_err(|error| format!("{error:#}"))?;
    let expires_at_ms = current_unix_time_ms().saturating_add(INTENT_DROP_CONFIRMATION_TTL_MS);
    sigil_kernel::IntentOperationAuthorityV1::new(
        digest,
        format!("tui-confirmed:{}", request.operation_id.as_str()),
        Some(expires_at_ms),
    )
    .map_err(|error| format!("{error:#}"))
}

fn send_intent_failure(message_tx: &mpsc::Sender<WorkerMessage>, request_id: u64, error: String) {
    let _ = message_tx.send(WorkerMessage::IntentStackOperationFailed { request_id, error });
}
