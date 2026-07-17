use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

pub(in crate::runner) struct ActiveMcpOAuthFlow {
    control_tx: tokio::sync::mpsc::Sender<sigil_runtime::McpOAuthFlowControl>,
    cancelled: Arc<AtomicBool>,
}

impl std::fmt::Debug for ActiveMcpOAuthFlow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ActiveMcpOAuthFlow")
            .field("cancelled", &self.cancelled.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

pub(in crate::runner) struct McpOAuthTaskResult {
    pub(in crate::runner) server_name: String,
    pub(in crate::runner) status: sigil_runtime::McpOAuthAuthStatus,
    pub(in crate::runner) activate_server: bool,
}

pub(in crate::runner) fn dispatch_mcp_oauth_action<P>(
    runtime: &tokio::runtime::Runtime,
    agent: &mut Arc<Agent<P>>,
    root_config: &RootConfig,
    message_tx: &mpsc::Sender<WorkerMessage>,
    state: &mut WorkerLoopState,
    server_name: String,
    action: McpOAuthUserAction,
) where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let Some(server) = root_config
        .mcp_servers
        .iter()
        .find(|server| server.name == server_name)
        .cloned()
    else {
        let _ = message_tx.send(WorkerMessage::Notice(format!(
            "unknown MCP server {server_name}"
        )));
        return;
    };

    if let McpOAuthUserAction::ManualCallback(callback) = action {
        let Some(active) = state.mcp_oauth.active.get(&server_name) else {
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "MCP {server_name} has no pending OAuth flow"
            )));
            return;
        };
        if active
            .control_tx
            .blocking_send(sigil_runtime::McpOAuthFlowControl::ManualCallback(callback))
            .is_err()
        {
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "MCP {server_name} OAuth flow is no longer active"
            )));
        }
        return;
    }
    if matches!(action, McpOAuthUserAction::Cancel) {
        let Some(active) = state.mcp_oauth.active.get(&server_name) else {
            return;
        };
        active.cancelled.store(true, Ordering::Release);
        let _ = active
            .control_tx
            .blocking_send(sigil_runtime::McpOAuthFlowControl::Cancel);
        return;
    }

    let Some(recorder) = state
        .session
        .current
        .as_ref()
        .and_then(|session| session.egress_audit_recorder().ok())
    else {
        let _ = message_tx.send(WorkerMessage::Notice(
            "MCP OAuth requires a durable session recorder".to_owned(),
        ));
        return;
    };
    let presenter: Arc<dyn sigil_kernel::EgressDisclosurePresenter> = Arc::new(
        crate::runner::egress_disclosure_bridge::ChannelEgressDisclosurePresenter::new(
            message_tx.clone(),
        ),
    );
    let cancelled = Arc::new(AtomicBool::new(false));
    let admission_is_live: Arc<dyn Fn() -> bool + Send + Sync> = {
        let cancelled = Arc::clone(&cancelled);
        Arc::new(move || !cancelled.load(Ordering::Acquire))
    };
    let executor = match sigil_runtime::runtime_mcp_oauth_executor_for_user_action(
        root_config,
        recorder,
        presenter,
        admission_is_live,
    ) {
        Ok(executor) => executor,
        Err(_) => {
            let _ = message_tx.send(WorkerMessage::Notice(
                "MCP OAuth network policy is invalid".to_owned(),
            ));
            return;
        }
    };
    let service = sigil_runtime::McpOAuthRuntimeService::new(
        Arc::new(sigil_runtime::McpOAuthCredentialManager::system()),
        executor,
    );
    let inspected = match runtime.block_on(service.inspect(&server)) {
        Ok(status) => status,
        Err(error) => {
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "MCP OAuth status failed: {error}"
            )));
            return;
        }
    };

    match action {
        McpOAuthUserAction::Inspect => {
            let _ = message_tx.send(WorkerMessage::McpOAuthStatus {
                status: inspected,
                revocation: None,
            });
        }
        McpOAuthUserAction::SignIn => {
            if state.mcp_oauth.active.contains_key(&server_name) {
                let _ = message_tx.send(WorkerMessage::Notice(format!(
                    "MCP {server_name} OAuth flow is already active"
                )));
                return;
            }
            let _ = message_tx.send(WorkerMessage::McpOAuthStatus {
                status: inspected
                    .clone()
                    .with_phase(sigil_runtime::McpOAuthAuthPhase::Discovering),
                revocation: None,
            });
            let flow = match runtime.block_on(service.begin(&server)) {
                Ok(flow) => flow,
                Err(error) => {
                    let _ = message_tx.send(WorkerMessage::McpOAuthStatus {
                        status: inspected.failed(&error),
                        revocation: None,
                    });
                    return;
                }
            };
            let prompt = flow.prompt();
            let _ = message_tx.send(WorkerMessage::McpOAuthStatus {
                status: prompt.clone(),
                revocation: None,
            });
            let (control_tx, control_rx) = tokio::sync::mpsc::channel(4);
            state.mcp_oauth.active.insert(
                server_name.clone(),
                ActiveMcpOAuthFlow {
                    control_tx,
                    cancelled,
                },
            );
            let result_tx = state.mcp_oauth.result_tx.clone();
            runtime.spawn(async move {
                let result = flow.run(control_rx).await;
                let (status, activate_server) = match result {
                    Ok(status) => (status, true),
                    Err(sigil_runtime::McpOAuthFlowError::Cancelled) => (prompt.cancelled(), false),
                    Err(error) => (prompt.failed(&error), false),
                };
                let _ = result_tx.send(McpOAuthTaskResult {
                    server_name,
                    status,
                    activate_server,
                });
            });
        }
        McpOAuthUserAction::Refresh => {
            let _ = message_tx.send(WorkerMessage::McpOAuthStatus {
                status: inspected
                    .clone()
                    .with_phase(sigil_runtime::McpOAuthAuthPhase::Refreshing),
                revocation: None,
            });
            let fingerprint = sigil_runtime::mcp_transport_static_fingerprint(&server)
                .unwrap_or_else(|_| "sha256:invalid".to_owned());
            let status = match runtime.block_on(service.refresh(&server, &fingerprint)) {
                Ok(status) => status,
                Err(error) => inspected.failed(&error),
            };
            let activate_server = status.phase == sigil_runtime::McpOAuthAuthPhase::SignedIn;
            let _ = state.mcp_oauth.result_tx.send(McpOAuthTaskResult {
                server_name,
                status,
                activate_server,
            });
        }
        McpOAuthUserAction::Revoke => {
            let _ = message_tx.send(WorkerMessage::McpOAuthStatus {
                status: inspected
                    .clone()
                    .with_phase(sigil_runtime::McpOAuthAuthPhase::Revoking),
                revocation: None,
            });
            match runtime.block_on(service.revoke(&server)) {
                Ok((status, revocation)) => {
                    let _ = message_tx.send(WorkerMessage::McpOAuthStatus {
                        status,
                        revocation: Some(revocation),
                    });
                }
                Err(error) => {
                    let _ = message_tx.send(WorkerMessage::McpOAuthStatus {
                        status: inspected.failed(&error),
                        revocation: None,
                    });
                }
            }
        }
        McpOAuthUserAction::ClearLocal => {
            let status = match runtime.block_on(service.clear_local(&server)) {
                Ok(status) => {
                    if let Some(agent) = Arc::get_mut(agent) {
                        match runtime.block_on(
                            sigil_runtime::deactivate_configured_remote_mcp_server(
                                agent.tool_registry_mut(),
                                &server_name,
                            ),
                        ) {
                            Ok(retired) if retired > 0 => {
                                let _ = message_tx.send(WorkerMessage::Notice(format!(
                                    "cleared MCP {server_name} credential and retired {retired} tool(s)"
                                )));
                            }
                            Ok(_) => {}
                            Err(error) => {
                                let _ = message_tx.send(WorkerMessage::Notice(format!(
                                    "MCP {server_name} credential was cleared; tool shutdown was incomplete: {error:#}"
                                )));
                            }
                        }
                    } else {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "MCP {server_name} credential was cleared; shared tools now fail authentication and will retire after the registry is released"
                        )));
                    }
                    status
                }
                Err(error) => inspected.failed(&error),
            };
            let _ = message_tx.send(WorkerMessage::McpOAuthStatus {
                status,
                revocation: None,
            });
        }
        McpOAuthUserAction::ManualCallback(_) | McpOAuthUserAction::Cancel => unreachable!(),
    }
}

pub(in crate::runner) fn advance_mcp_oauth_results(
    message_tx: &mpsc::Sender<WorkerMessage>,
    state: &mut WorkerLoopState,
) -> bool {
    let mut advanced = false;
    while let Ok(result) = state.mcp_oauth.result_rx.try_recv() {
        advanced = true;
        state.mcp_oauth.active.remove(&result.server_name);
        let _ = message_tx.send(WorkerMessage::McpOAuthStatus {
            status: result.status,
            revocation: None,
        });
        if result.activate_server {
            state.refresh.pending_mcp_servers.insert(result.server_name);
            state.refresh.next_mcp_retry_at = Instant::now();
        }
    }
    advanced
}

pub(in crate::runner) fn cancel_all_mcp_oauth_flows(state: &mut WorkerLoopState) {
    for active in state.mcp_oauth.active.values() {
        active.cancelled.store(true, Ordering::Release);
        let _ = active
            .control_tx
            .blocking_send(sigil_runtime::McpOAuthFlowControl::Cancel);
    }
    state.mcp_oauth.active.clear();
}
