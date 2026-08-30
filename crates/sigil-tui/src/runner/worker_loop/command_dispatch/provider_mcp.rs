use super::*;

pub(super) fn dispatch_provider_mcp_command<P>(
    context: WorkerCommandContext<'_, P>,
    command: ProviderMcpCommand,
) -> WorkerCommandDispatchControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerCommandContext {
        runtime,
        agent,
        root_config,
        config_path: _,
        provider_capabilities,
        workspace_root: _,
        options,
        permission_mode_override: _,
        message_tx,
        elicitation_handler,
        mcp_event_handler,
        role_provider_builder: _,
        context_resolver: _,
        managed_extension_execution,
        managed_verification_execution: _,
        state,
    } = context;
    let mut command_result = Some(command);
    let control = WorkerCommandDispatchControl::Continue;
    while let Some(command_result) = command_result.take() {
        match command_result {
            ProviderMcpCommand::RefreshProviderBalance {
                request_id,
                provider_config,
            } => {
                let result_tx = provider_status_result_sender(runtime, &state.event_tx);
                state.refresh.provider_status_tasks.refresh_balance(
                    runtime,
                    request_id,
                    provider_config,
                    result_tx,
                );
            }
            ProviderMcpCommand::RefreshProviderModels {
                request_id,
                provider_config,
            } => {
                let result_tx = provider_status_result_sender(runtime, &state.event_tx);
                state.refresh.provider_status_tasks.refresh_models(
                    runtime,
                    request_id,
                    provider_config,
                    result_tx,
                );
            }
            ProviderMcpCommand::RefreshConnectionModels {
                cache_root,
                root_config,
                request,
                prepared_credential,
            } => {
                let result_tx = provider_status_result_sender(runtime, &state.event_tx);
                state
                    .refresh
                    .provider_status_tasks
                    .refresh_connection_models(
                        runtime,
                        cache_root,
                        *root_config,
                        request,
                        prepared_credential,
                        result_tx,
                    );
            }
            ProviderMcpCommand::CancelProviderModelsRefresh { request_id } => {
                state
                    .refresh
                    .provider_status_tasks
                    .cancel_models_refresh(request_id);
            }
            ProviderMcpCommand::ActivateLazyMcp { server_name } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::LocalOperationOutcome(
                        LocalOperationOutcome::rejected(
                            "mcp.activate",
                            LocalOperationKind::McpActivation,
                            "cannot activate MCP while the agent is running",
                        ),
                    ));
                    continue;
                }
                let Some(agent) = Arc::get_mut(agent) else {
                    let _ = message_tx.send(WorkerMessage::LocalOperationOutcome(
                        LocalOperationOutcome::deferred(
                            "mcp.activate",
                            LocalOperationKind::McpActivation,
                            "cannot activate MCP while agent registry is shared",
                        ),
                    ));
                    continue;
                };
                let _ = message_tx.send(WorkerMessage::McpActivationStatus {
                    server_name: server_name.clone(),
                    status: McpActivationStatus::Activating,
                });
                let mutation_recorder = state
                    .session
                    .current
                    .as_ref()
                    .and_then(Session::mutation_event_recorder);
                let egress_recorder = state
                    .session
                    .current
                    .as_ref()
                    .and_then(|session| session.egress_audit_recorder().ok());
                let disclosure_presenter: Arc<dyn sigil_kernel::EgressDisclosurePresenter> =
                    Arc::new(
                        crate::runner::egress_disclosure_bridge::ChannelEgressDisclosurePresenter::new(
                            message_tx.clone(),
                        ),
                    );
                match runtime.block_on(sigil_runtime::activate_mcp_tools_from_product_surface_with_managed_extension_execution(
                    agent.tool_registry_mut(),
                    root_config,
                    provider_capabilities,
                    options.workspace_root.clone(),
                    server_name.as_deref(),
                    elicitation_handler.clone(),
                    mcp_event_handler.clone(),
                    mutation_recorder,
                    sigil_kernel::ExtensionProcessNetworkAdmission::new(
                        options.permission_context.network_policy,
                        false,
                    ),
                    egress_recorder,
                    disclosure_presenter,
                    managed_extension_execution.as_ref().map(Arc::clone),
                )) {
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
                            status: McpActivationStatus::from_error(error.clone()),
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
            ProviderMcpCommand::RefreshMcpServer { server_name } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::LocalOperationOutcome(
                        LocalOperationOutcome::rejected(
                            "mcp.refresh",
                            LocalOperationKind::McpRefresh,
                            "cannot refresh MCP while the agent is running",
                        ),
                    ));
                    continue;
                }
                state.refresh.pending_mcp_servers.insert(server_name);
                state.refresh.next_mcp_retry_at = Instant::now();
            }
            ProviderMcpCommand::McpOAuth {
                server_name,
                action,
            } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::LocalOperationOutcome(
                        LocalOperationOutcome::rejected(
                            "mcp.authentication",
                            LocalOperationKind::McpAuthentication,
                            "cannot manage MCP authentication while the agent is running",
                        ),
                    ));
                    continue;
                }
                dispatch_mcp_oauth_action(
                    runtime,
                    agent,
                    root_config,
                    message_tx,
                    state,
                    server_name,
                    action,
                );
            }
        }
    }
    control
}
