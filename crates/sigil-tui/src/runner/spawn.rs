use std::{
    path::PathBuf,
    sync::{Arc, mpsc},
    thread,
};

use anyhow::{Context, Result};
use sigil_kernel::{Agent, InteractionMode, RootConfig};
use tokio::runtime::Runtime;

use super::{
    elicitation_bridge::ChannelMcpElicitationHandler,
    protocol::{WorkerCommand, WorkerMessage},
    worker_loop::run_worker_loop,
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
            let registry =
                match runtime.block_on(sigil_runtime::build_tool_registry_with_mcp_elicitation(
                    &root_config,
                    &provider_capabilities,
                    workspace_root.clone(),
                    elicitation_handler.clone(),
                )) {
                    Ok(registry) => registry,
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                        return;
                    }
                };
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
                elicitation_handler,
            );
        })
        .context("failed to spawn sigil agent worker")?;

    Ok((command_tx, message_rx))
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
