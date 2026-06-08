use std::{
    path::PathBuf,
    sync::{Arc, mpsc},
    thread,
};

use anyhow::{Context, Result};
use termquill_kernel::{Agent, InteractionMode, RootConfig};

use super::{
    protocol::{WorkerCommand, WorkerMessage},
    worker_loop::run_worker_loop,
};

pub fn spawn_agent_worker(
    root_config: RootConfig,
    session_log_path: PathBuf,
    workspace_root: PathBuf,
) -> Result<(mpsc::Sender<WorkerCommand>, mpsc::Receiver<WorkerMessage>)> {
    let (command_tx, command_rx) = mpsc::channel();
    let (message_tx, message_rx) = mpsc::channel();

    let options = termquill_runtime::build_run_options(
        &root_config,
        workspace_root.clone(),
        InteractionMode::Interactive,
    );

    thread::Builder::new()
        .name("termquill-agent-worker".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                    return;
                }
            };

            let registry =
                match runtime.block_on(termquill_runtime::build_tool_registry(&root_config)) {
                    Ok(registry) => registry,
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                        return;
                    }
                };

            let provider = match termquill_runtime::build_provider(&root_config) {
                Ok(provider) => provider,
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
                session_log_path,
                options,
                command_rx,
                message_tx,
            );
        })
        .context("failed to spawn termquill agent worker")?;

    Ok((command_tx, message_rx))
}
