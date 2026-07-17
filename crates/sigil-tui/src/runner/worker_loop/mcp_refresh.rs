use super::*;

pub(in crate::runner) struct WorkerLoopMcpHandlers {
    pub(in crate::runner) elicitation_handler: Arc<ChannelMcpElicitationHandler>,
    pub(in crate::runner) event_handler: Arc<ChannelMcpRuntimeEventHandler>,
    pub(in crate::runner) event_rx: mpsc::Receiver<McpRuntimeEvent>,
    pub(in crate::runner) role_provider_builder: Arc<dyn TaskRoleProviderBuilder>,
    pub(in crate::runner) context_resolver: sigil_runtime::RequestContextResolver,
}

#[allow(clippy::too_many_arguments)]
pub(in crate::runner) fn refresh_pending_mcp_servers<P>(
    runtime: &tokio::runtime::Runtime,
    agent: &mut Arc<Agent<P>>,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    options: &AgentRunOptions,
    message_tx: &mpsc::Sender<WorkerMessage>,
    elicitation_handler: Arc<ChannelMcpElicitationHandler>,
    mcp_event_handler: Arc<ChannelMcpRuntimeEventHandler>,
    mutation_recorder: Option<MutationEventRecorder>,
    egress_recorder: Option<sigil_kernel::EgressAuditRecorder>,
    pending_mcp_refreshes: &mut BTreeSet<String>,
) -> bool
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let servers = std::mem::take(pending_mcp_refreshes);
    let mut shared_registry_blocked = false;
    for server_name in servers {
        let Some(agent) = Arc::get_mut(agent) else {
            pending_mcp_refreshes.insert(server_name.clone());
            shared_registry_blocked = true;
            let _ = message_tx.send(WorkerMessage::RunFailed(
                "cannot refresh MCP while agent registry is shared".to_owned(),
            ));
            continue;
        };
        let _ = message_tx.send(WorkerMessage::McpActivationStatus {
            server_name: Some(server_name.clone()),
            status: McpActivationStatus::Refreshing,
        });
        let elicitation_handler_trait: Arc<dyn sigil_runtime::McpElicitationHandler> =
            elicitation_handler.clone();
        let mcp_event_handler_trait: Arc<dyn sigil_runtime::McpRuntimeEventHandler> =
            mcp_event_handler.clone();
        let disclosure_presenter: Arc<dyn sigil_kernel::EgressDisclosurePresenter> = Arc::new(
            crate::runner::egress_disclosure_bridge::ChannelEgressDisclosurePresenter::new(
                message_tx.clone(),
            ),
        );
        match runtime.block_on(
            sigil_runtime::refresh_mcp_server_tools_from_product_surface(
                agent.tool_registry_mut(),
                root_config,
                provider_capabilities,
                options.workspace_root.clone(),
                &server_name,
                elicitation_handler_trait,
                mcp_event_handler_trait,
                mutation_recorder.clone(),
                sigil_kernel::ExtensionProcessNetworkAdmission::new(
                    options.permission_context.network_policy,
                    false,
                ),
                egress_recorder.clone(),
                disclosure_presenter,
            ),
        ) {
            Ok(result) if result.matched_servers == 0 => {
                let _ = message_tx.send(WorkerMessage::McpActivationStatus {
                    server_name: Some(server_name.clone()),
                    status: McpActivationStatus::Deferred,
                });
                let _ = message_tx.send(WorkerMessage::Notice(format!(
                    "MCP refresh skipped for unknown server {server_name}"
                )));
            }
            Ok(result) => {
                let _ = message_tx.send(WorkerMessage::McpActivationStatus {
                    server_name: Some(server_name.clone()),
                    status: McpActivationStatus::Ready {
                        added_tools: result.added_tools,
                        process_coverage: sigil_runtime::mcp_process_receipts_summary(
                            &result.process_launch_receipts,
                        ),
                    },
                });
                let _ = message_tx.send(WorkerMessage::Notice(format!(
                    "refreshed {} MCP tools for {server_name}",
                    result.added_tools
                )));
            }
            Err(error) => {
                let error = format!("{error:#}");
                let _ = message_tx.send(WorkerMessage::McpActivationStatus {
                    server_name: Some(server_name.clone()),
                    status: McpActivationStatus::from_error(error.clone()),
                });
                let _ = message_tx.send(WorkerMessage::Notice(format!(
                    "MCP refresh failed for {server_name}: {error}"
                )));
            }
        }
    }
    shared_registry_blocked
}
