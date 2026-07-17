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
        provider_capabilities,
        workspace_root: _,
        options,
        message_tx,
        elicitation_handler,
        mcp_event_handler,
        role_provider_builder: _,
        context_resolver: _,
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
                state.refresh.provider_status_tasks.refresh_balance(
                    runtime,
                    request_id,
                    provider_config,
                    state.refresh.provider_status_tx.clone(),
                );
            }
            ProviderMcpCommand::RefreshProviderModels {
                request_id,
                provider_config,
            } => {
                state.refresh.provider_status_tasks.refresh_models(
                    runtime,
                    request_id,
                    provider_config,
                    state.refresh.provider_status_tx.clone(),
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
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "cannot activate MCP while the agent is running".to_owned(),
                    ));
                    continue;
                }
                let Some(agent) = Arc::get_mut(agent) else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "cannot activate MCP while agent registry is shared".to_owned(),
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
                match runtime.block_on(sigil_runtime::activate_mcp_tools_from_product_surface(
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
            ProviderMcpCommand::RefreshMcpServer { server_name } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "cannot refresh MCP while the agent is running".to_owned(),
                    ));
                    continue;
                }
                state.refresh.pending_mcp_servers.insert(server_name);
                state.refresh.next_mcp_retry_at = Instant::now();
            }
        }
    }
    control
}
