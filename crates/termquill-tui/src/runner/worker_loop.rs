use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{Arc, mpsc},
    time::Duration,
};

use termquill_kernel::{
    Agent, AgentRunOptions, AgentRunResult, ProviderCapabilities, RootConfig, Session, ToolApproval,
};

use crate::context_window::effective_compaction_config;

use super::{
    approval_bridge::{ApprovalSignal, ChannelApprovalHandler},
    diagnostics::{changed_source_files, check_changed_files_diagnostics, diagnostics_tool_event},
    event_bridge::ChannelEventHandler,
    protocol::{CompactionTrigger, WorkerCommand, WorkerMessage},
    session_flow::{auto_compact_session, load_session, session_compacted_message},
};

pub(super) fn run_worker_loop<P>(
    runtime: tokio::runtime::Runtime,
    mut agent: Arc<Agent<P>>,
    root_config: RootConfig,
    provider_capabilities: ProviderCapabilities,
    session_log_path: PathBuf,
    options: AgentRunOptions,
    command_rx: mpsc::Receiver<WorkerCommand>,
    message_tx: mpsc::Sender<WorkerMessage>,
) where
    P: termquill_kernel::Provider + Send + Sync + 'static,
{
    let mut current_session_log_path = session_log_path;
    let mut current_session = match load_session(
        &root_config.agent.provider,
        &root_config.agent.model,
        &current_session_log_path,
    ) {
        Ok(session) => Some(session),
        Err(error) => {
            let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
            return;
        }
    };

    let (task_result_tx, task_result_rx) = mpsc::channel::<RunTaskResult>();
    let mut active_run: Option<ActiveRun> = None;
    let mut next_run_id = 1_u64;
    let mut discarded_run_ids = BTreeSet::new();

    loop {
        while let Ok(task_result) = task_result_rx.try_recv() {
            if discarded_run_ids.remove(&task_result.run_id) {
                continue;
            }
            active_run = None;
            current_session = Some(task_result.session);
            let auto_compaction = match current_session.as_mut() {
                Some(session) => {
                    let effective_config = effective_compaction_config(
                        session.provider_name(),
                        session.model_name(),
                        &options.compaction_config,
                    );
                    match auto_compact_session(session, &effective_config) {
                        Ok(record) => record,
                        Err(error) => {
                            let _ = message_tx.send(WorkerMessage::Notice(format!(
                                "automatic compaction skipped: {error}",
                            )));
                            None
                        }
                    }
                }
                None => None,
            };
            match task_result.result {
                Ok(run_result) => {
                    let entries = current_session
                        .as_ref()
                        .map(|session| session.entries().to_vec())
                        .unwrap_or_default();
                    let _ = message_tx.send(WorkerMessage::RunFinished {
                        result: run_result,
                        entries,
                    });
                }
                Err(error) => {
                    let _ = message_tx.send(WorkerMessage::RunFailed(error));
                }
            }
            if let (Some(session), Some(record)) = (current_session.as_ref(), auto_compaction) {
                let _ = message_tx.send(session_compacted_message(
                    &current_session_log_path,
                    session,
                    record,
                    CompactionTrigger::AutomaticHardThreshold,
                ));
            }
        }

        match command_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(WorkerCommand::SubmitPrompt {
                prompt,
                reasoning_effort,
            }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "agent is already running".to_owned(),
                    ));
                    continue;
                }

                let Some(run_session) = current_session.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };

                let _ = message_tx.send(WorkerMessage::RunStarted {
                    prompt: prompt.clone(),
                });

                let mut handler = ChannelEventHandler::new(message_tx.clone());
                let (approval_tx, approval_rx) = mpsc::channel();
                let agent = Arc::clone(&agent);
                let mut options = options.clone();
                options.reasoning_effort = Some(reasoning_effort);
                let task_result_tx = task_result_tx.clone();
                let run_id = next_run_id;
                next_run_id += 1;

                let handle = runtime.spawn(async move {
                    let mut run_session = run_session;
                    let result = {
                        let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
                        agent
                            .run_with_approval(
                                &mut run_session,
                                prompt,
                                options,
                                &mut handler,
                                &mut approval_handler,
                            )
                            .await
                            .map_err(|error| format!("{error:#}"))
                    };
                    let _ = task_result_tx.send(RunTaskResult {
                        run_id,
                        session: run_session,
                        result,
                    });
                });

                active_run = Some(ActiveRun {
                    run_id,
                    handle,
                    approval_tx,
                });
            }
            Ok(WorkerCommand::ApprovalDecision { call_id, approved }) => {
                if let Some(active_run) = &active_run {
                    let approval = if approved {
                        ToolApproval::Approve
                    } else {
                        ToolApproval::Deny {
                            reason: "denied in TUI".to_owned(),
                        }
                    };
                    let _ = active_run
                        .approval_tx
                        .send(ApprovalSignal::Decision { call_id, approval });
                } else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "received stray approval decision without pending approval".to_owned(),
                    ));
                }
            }
            Ok(WorkerCommand::CancelRun) => {
                if let Some(active_run) = active_run.take() {
                    discarded_run_ids.insert(active_run.run_id);
                    let _ = active_run.approval_tx.send(ApprovalSignal::Cancel);
                    active_run.handle.abort();
                    match load_session(
                        &root_config.agent.provider,
                        &root_config.agent.model,
                        &current_session_log_path,
                    ) {
                        Ok(session) => {
                            let entries = session.entries().to_vec();
                            current_session = Some(session);
                            let _ = message_tx.send(WorkerMessage::RunCancelled {
                                session_log_path: current_session_log_path.clone(),
                                provider_name: current_session
                                    .as_ref()
                                    .map(|session| session.provider_name().to_owned())
                                    .unwrap_or_else(|| root_config.agent.provider.clone()),
                                model_name: current_session
                                    .as_ref()
                                    .map(|session| session.model_name().to_owned())
                                    .unwrap_or_else(|| root_config.agent.model.clone()),
                                entries,
                            });
                        }
                        Err(error) => {
                            let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                        }
                    }
                } else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "no active run to cancel".to_owned(),
                    ));
                }
            }
            Ok(WorkerCommand::CompactNow) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "cannot compact while the agent is running".to_owned(),
                    ));
                    continue;
                }
                let Some(mut session) = current_session.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };
                let effective_config = effective_compaction_config(
                    session.provider_name(),
                    session.model_name(),
                    &options.compaction_config,
                );
                match session.compact_now(&effective_config) {
                    Ok(record) => {
                        current_session = Some(session);
                        if let Some(session) = current_session.as_ref() {
                            let _ = message_tx.send(session_compacted_message(
                                &current_session_log_path,
                                session,
                                record,
                                CompactionTrigger::Manual,
                            ));
                        }
                    }
                    Err(error) => {
                        current_session = Some(session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                    }
                }
            }
            Ok(WorkerCommand::CheckChangedFilesDiagnostics) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "cannot check changes while the agent is running".to_owned(),
                    ));
                    continue;
                }
                let changed_paths = match changed_source_files(&options.workspace_root) {
                    Ok(paths) => paths,
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                        continue;
                    }
                };
                if changed_paths.is_empty() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "no changed source files to check".to_owned(),
                    ));
                    continue;
                }
                let Some(session) = current_session.as_mut() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };
                match check_changed_files_diagnostics(
                    &runtime,
                    agent.tool_registry(),
                    session,
                    &options,
                    root_config.code_intelligence.max_results,
                    changed_paths,
                ) {
                    Ok(result) => {
                        let _ = message_tx.send(WorkerMessage::Event(Box::new(
                            diagnostics_tool_event(result),
                        )));
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                    }
                }
            }
            Ok(WorkerCommand::ActivateLazyMcp { server_name }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "cannot activate MCP while the agent is running".to_owned(),
                    ));
                    continue;
                }
                let Some(agent) = Arc::get_mut(&mut agent) else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "cannot activate MCP while agent registry is shared".to_owned(),
                    ));
                    continue;
                };
                match runtime.block_on(termquill_runtime::activate_lazy_mcp_tools(
                    agent.tool_registry_mut(),
                    &root_config,
                    &provider_capabilities,
                    options.workspace_root.clone(),
                    server_name.as_deref(),
                )) {
                    Ok(0) => {
                        let detail = server_name
                            .as_deref()
                            .map(|name| format!(" for {name}"))
                            .unwrap_or_default();
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "no lazy MCP tools activated{detail}"
                        )));
                    }
                    Ok(count) => {
                        let detail = server_name
                            .as_deref()
                            .map(|name| format!(" for {name}"))
                            .unwrap_or_default();
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "activated {count} lazy MCP tools{detail}"
                        )));
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                    }
                }
            }
            Ok(WorkerCommand::SwitchSession { session_log_path }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "cannot switch sessions while the agent is running".to_owned(),
                    ));
                    continue;
                }

                match load_session(
                    &root_config.agent.provider,
                    &root_config.agent.model,
                    &session_log_path,
                ) {
                    Ok(session) => {
                        let entries = session.entries().to_vec();
                        current_session_log_path = session_log_path.clone();
                        let provider_name = session.provider_name().to_owned();
                        let model_name = session.model_name().to_owned();
                        current_session = Some(session);
                        let _ = message_tx.send(WorkerMessage::SessionSwitched {
                            session_log_path,
                            provider_name,
                            model_name,
                            entries,
                        });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                    }
                }
            }
            Ok(WorkerCommand::Shutdown) => {
                if let Some(active_run) = active_run.take() {
                    discarded_run_ids.insert(active_run.run_id);
                    let _ = active_run.approval_tx.send(ApprovalSignal::Cancel);
                    active_run.handle.abort();
                }
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

struct ActiveRun {
    run_id: u64,
    handle: tokio::task::JoinHandle<()>,
    approval_tx: mpsc::Sender<ApprovalSignal>,
}

struct RunTaskResult {
    run_id: u64,
    session: Session,
    result: std::result::Result<AgentRunResult, String>,
}
