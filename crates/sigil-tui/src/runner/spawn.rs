use std::{
    path::PathBuf,
    sync::{Arc, mpsc},
    thread,
};

use anyhow::{Context, Result};
use sigil_kernel::{Agent, InteractionMode, McpServerStartup, ProviderCapabilities, RootConfig};
use sigil_runtime::{McpElicitationHandler, McpRuntimeEventHandler};
use tokio::runtime::Runtime;

use super::{
    elicitation_bridge::ChannelMcpElicitationHandler,
    mcp_event_bridge::ChannelMcpRuntimeEventHandler,
    protocol::{McpActivationStatus, WorkerCommand, WorkerMessage},
    worker_loop::{WorkerLoopMcpHandlers, run_worker_loop},
};

pub fn spawn_agent_worker(
    root_config: RootConfig,
    session_log_path: PathBuf,
    workspace_root: PathBuf,
    interaction_mode: InteractionMode,
) -> Result<(mpsc::Sender<WorkerCommand>, mpsc::Receiver<WorkerMessage>)> {
    let (command_tx, command_rx) = mpsc::channel();
    let (message_tx, message_rx) = mpsc::channel();

    let options =
        sigil_runtime::build_run_options(&root_config, workspace_root.clone(), interaction_mode);

    thread::Builder::new()
        .name("sigil-agent-worker".to_owned())
        .spawn(move || {
            let Some(runtime) = report_runtime_build_result(build_worker_runtime(), &message_tx)
            else {
                return;
            };

            let provider = match sigil_runtime::build_provider(&root_config) {
                Ok(provider) => provider,
                Err(error) => {
                    let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                    return;
                }
            };
            let provider_capabilities = provider.capabilities();
            let elicitation_handler =
                Arc::new(ChannelMcpElicitationHandler::new(message_tx.clone()));
            let (mcp_event_tx, mcp_event_rx) = mpsc::channel();
            let mcp_event_handler = Arc::new(ChannelMcpRuntimeEventHandler::new(mcp_event_tx));
            let mut registry = sigil_runtime::build_tool_registry_without_eager_mcp(
                &root_config,
                &provider_capabilities,
                workspace_root.clone(),
                elicitation_handler.clone(),
                mcp_event_handler.clone(),
            );
            if let Err(error) = sigil_runtime::register_agent_tools(&mut registry, &root_config) {
                let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                return;
            }
            spawn_eager_mcp_startup_tasks(
                &runtime,
                registry.clone(),
                &root_config,
                &provider_capabilities,
                workspace_root.clone(),
                &message_tx,
                elicitation_handler.clone(),
                mcp_event_handler.clone(),
            );
            let agent = Arc::new(Agent::new(provider, registry));
            run_worker_loop(
                runtime,
                agent,
                root_config,
                provider_capabilities,
                session_log_path,
                options,
                command_rx,
                message_tx,
                WorkerLoopMcpHandlers {
                    elicitation_handler,
                    event_handler: mcp_event_handler,
                    event_rx: mcp_event_rx,
                },
            );
        })
        .context("failed to spawn sigil agent worker")?;

    Ok((command_tx, message_rx))
}

#[allow(clippy::too_many_arguments)]
fn spawn_eager_mcp_startup_tasks(
    runtime: &Runtime,
    registry: sigil_kernel::ToolRegistry,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    message_tx: &mpsc::Sender<WorkerMessage>,
    elicitation_handler: Arc<ChannelMcpElicitationHandler>,
    mcp_event_handler: Arc<ChannelMcpRuntimeEventHandler>,
) {
    for server in root_config
        .mcp_servers
        .iter()
        .filter(|server| server.startup == McpServerStartup::Eager)
    {
        let server_name = server.name.clone();
        let _ = message_tx.send(WorkerMessage::McpActivationStatus {
            server_name: Some(server_name.clone()),
            status: McpActivationStatus::Activating,
        });

        let mut registry = registry.clone();
        let mut root_config = root_config.clone();
        for configured in &mut root_config.mcp_servers {
            if configured.name == server_name {
                configured.required = true;
            }
        }
        let provider_capabilities = provider_capabilities.clone();
        let workspace_root = workspace_root.clone();
        let message_tx = message_tx.clone();
        let elicitation_handler: Arc<dyn McpElicitationHandler> = elicitation_handler.clone();
        let mcp_event_handler: Arc<dyn McpRuntimeEventHandler> = mcp_event_handler.clone();

        runtime.spawn(async move {
            match sigil_runtime::refresh_mcp_server_tools_with_mcp_handlers(
                &mut registry,
                &root_config,
                &provider_capabilities,
                workspace_root,
                &server_name,
                elicitation_handler,
                mcp_event_handler,
            )
            .await
            {
                Ok(result) => {
                    let _ = message_tx.send(WorkerMessage::McpActivationStatus {
                        server_name: Some(server_name.clone()),
                        status: McpActivationStatus::Ready {
                            added_tools: result.added_tools,
                        },
                    });
                }
                Err(error) => {
                    let error = format!("{error:#}");
                    let _ = message_tx.send(WorkerMessage::McpActivationStatus {
                        server_name: Some(server_name.clone()),
                        status: McpActivationStatus::Failed {
                            error: error.clone(),
                        },
                    });
                    let _ = message_tx.send(WorkerMessage::Notice(format!(
                        "MCP startup failed for {server_name}: {error}"
                    )));
                }
            }
        });
    }
}

fn build_worker_runtime() -> Result<Runtime, std::io::Error> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
}

pub(super) fn report_runtime_build_result(
    result: Result<Runtime, std::io::Error>,
    message_tx: &mpsc::Sender<WorkerMessage>,
) -> Option<Runtime> {
    match result {
        Ok(runtime) => Some(runtime),
        Err(error) => {
            let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
            None
        }
    }
}
