use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{Arc, mpsc},
    time::Duration,
};

use sigil_kernel::{
    Agent, AgentRunOptions, AgentRunResult, ProviderCapabilities, RootConfig, Session, ToolApproval,
};

use crate::{
    context_window::effective_compaction_config,
    provider_status::{BalanceSnapshot, fetch_provider_balance_snapshot, fetch_remote_model_ids},
};

use super::{
    approval_bridge::{ApprovalSignal, ChannelApprovalHandler},
    diagnostics::{changed_source_files, check_changed_files_diagnostics, diagnostics_tool_event},
    elicitation_bridge::{ChannelMcpElicitationHandler, McpElicitationAuditBuffer},
    event_bridge::ChannelEventHandler,
    mcp_event_bridge::{ChannelMcpRuntimeEventHandler, McpRuntimeEvent},
    protocol::{CompactionTrigger, McpActivationStatus, WorkerCommand, WorkerMessage},
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
    mcp_handlers: WorkerLoopMcpHandlers,
) where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerLoopMcpHandlers {
        elicitation_handler,
        event_handler: mcp_event_handler,
        event_rx: mcp_event_rx,
    } = mcp_handlers;
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
    let (provider_status_tx, provider_status_rx) = mpsc::channel::<ProviderStatusTaskResult>();
    let mut active_run: Option<ActiveRun> = None;
    let mut active_balance_refresh: Option<ActiveProviderStatusTask> = None;
    let mut active_model_refresh: Option<ActiveProviderStatusTask> = None;
    let mut next_run_id = 1_u64;
    let mut discarded_run_ids = BTreeSet::new();
    let mut pending_mcp_refreshes = BTreeSet::new();

    loop {
        while let Ok(event) = mcp_event_rx.try_recv() {
            match event {
                McpRuntimeEvent::Progress(notification) => {
                    let _ = message_tx.send(WorkerMessage::McpProgress { notification });
                }
                McpRuntimeEvent::ListChanged(notification) => {
                    pending_mcp_refreshes.insert(notification.server_name.clone());
                    let _ = message_tx.send(WorkerMessage::McpListChanged { notification });
                }
            }
        }

        if active_run.is_none() && !pending_mcp_refreshes.is_empty() {
            refresh_pending_mcp_servers(
                &runtime,
                &mut agent,
                &root_config,
                &provider_capabilities,
                &options,
                &message_tx,
                Arc::clone(&elicitation_handler),
                Arc::clone(&mcp_event_handler),
                &mut pending_mcp_refreshes,
            );
        }

        while let Ok(status_result) = provider_status_rx.try_recv() {
            match status_result {
                ProviderStatusTaskResult::Balance {
                    request_id,
                    snapshot,
                } => {
                    if active_balance_refresh
                        .as_ref()
                        .is_some_and(|task| task.request_id == request_id)
                    {
                        active_balance_refresh = None;
                        let _ = message_tx.send(WorkerMessage::ProviderBalanceRefreshed {
                            request_id,
                            snapshot,
                        });
                    }
                }
                ProviderStatusTaskResult::Models {
                    request_id,
                    base_url,
                    result,
                } => {
                    if active_model_refresh
                        .as_ref()
                        .is_some_and(|task| task.request_id == request_id)
                    {
                        active_model_refresh = None;
                        let _ = message_tx.send(WorkerMessage::ProviderModelsRefreshed {
                            request_id,
                            base_url,
                            result,
                        });
                    }
                }
            }
        }

        while let Ok(task_result) = task_result_rx.try_recv() {
            if discarded_run_ids.remove(&task_result.run_id) {
                continue;
            }
            elicitation_handler.set_audit_buffer(None);
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
                let elicitation_audit_buffer: McpElicitationAuditBuffer =
                    Arc::new(std::sync::Mutex::new(Vec::new()));
                elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
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
                    let result = match append_mcp_elicitation_audits(
                        &mut run_session,
                        &run_elicitation_audit_buffer,
                    ) {
                        Ok(()) => result,
                        Err(error) => Err(error),
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
                    elicitation_audit_buffer,
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
                    elicitation_handler.set_audit_buffer(None);
                    discarded_run_ids.insert(active_run.run_id);
                    let _ = active_run.approval_tx.send(ApprovalSignal::Cancel);
                    active_run.handle.abort();
                    match load_session(
                        &root_config.agent.provider,
                        &root_config.agent.model,
                        &current_session_log_path,
                    ) {
                        Ok(session) => {
                            let mut session = session;
                            if let Err(error) = append_mcp_elicitation_audits(
                                &mut session,
                                &active_run.elicitation_audit_buffer,
                            ) {
                                let _ = message_tx.send(WorkerMessage::RunFailed(error));
                                current_session = Some(session);
                                continue;
                            }
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
            Ok(WorkerCommand::RefreshProviderBalance {
                request_id,
                provider_config,
            }) => {
                if let Some(task) = active_balance_refresh.take() {
                    task.handle.abort();
                }
                let provider_status_tx = provider_status_tx.clone();
                let handle = runtime.spawn(async move {
                    let snapshot = fetch_provider_balance_snapshot(&provider_config)
                        .await
                        .unwrap_or(BalanceSnapshot {
                            status: "balance unavailable".to_owned(),
                            ..BalanceSnapshot::default()
                        });
                    let _ = provider_status_tx.send(ProviderStatusTaskResult::Balance {
                        request_id,
                        snapshot,
                    });
                });
                active_balance_refresh = Some(ActiveProviderStatusTask { request_id, handle });
            }
            Ok(WorkerCommand::RefreshProviderModels {
                request_id,
                provider_config,
            }) => {
                if let Some(task) = active_model_refresh.take() {
                    task.handle.abort();
                }
                let base_url = provider_config.base_url.clone();
                let provider_status_tx = provider_status_tx.clone();
                let handle = runtime.spawn(async move {
                    let result = fetch_remote_model_ids(&provider_config)
                        .await
                        .map_err(|error| format!("{error:#}"));
                    let _ = provider_status_tx.send(ProviderStatusTaskResult::Models {
                        request_id,
                        base_url,
                        result,
                    });
                });
                active_model_refresh = Some(ActiveProviderStatusTask { request_id, handle });
            }
            Ok(WorkerCommand::CancelProviderModelsRefresh { request_id }) => {
                if active_model_refresh
                    .as_ref()
                    .is_some_and(|task| task.request_id == request_id)
                    && let Some(task) = active_model_refresh.take()
                {
                    task.handle.abort();
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
                let _ = message_tx.send(WorkerMessage::McpActivationStatus {
                    server_name: server_name.clone(),
                    status: McpActivationStatus::Activating,
                });
                match runtime.block_on(
                    sigil_runtime::activate_lazy_mcp_tools_detailed_with_mcp_handlers(
                        agent.tool_registry_mut(),
                        &root_config,
                        &provider_capabilities,
                        options.workspace_root.clone(),
                        server_name.as_deref(),
                        elicitation_handler.clone(),
                        mcp_event_handler.clone(),
                    ),
                ) {
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
                    elicitation_handler.set_audit_buffer(None);
                    discarded_run_ids.insert(active_run.run_id);
                    let _ = active_run.approval_tx.send(ApprovalSignal::Cancel);
                    active_run.handle.abort();
                }
                if let Some(task) = active_balance_refresh.take() {
                    task.handle.abort();
                }
                if let Some(task) = active_model_refresh.take() {
                    task.handle.abort();
                }
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    if let Some(task) = active_balance_refresh.take() {
        task.handle.abort();
    }
    if let Some(task) = active_model_refresh.take() {
        task.handle.abort();
    }
}

pub(super) struct WorkerLoopMcpHandlers {
    pub(super) elicitation_handler: Arc<ChannelMcpElicitationHandler>,
    pub(super) event_handler: Arc<ChannelMcpRuntimeEventHandler>,
    pub(super) event_rx: mpsc::Receiver<McpRuntimeEvent>,
}

struct ActiveRun {
    run_id: u64,
    handle: tokio::task::JoinHandle<()>,
    approval_tx: mpsc::Sender<ApprovalSignal>,
    elicitation_audit_buffer: McpElicitationAuditBuffer,
}

#[allow(clippy::too_many_arguments)]
fn refresh_pending_mcp_servers<P>(
    runtime: &tokio::runtime::Runtime,
    agent: &mut Arc<Agent<P>>,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    options: &AgentRunOptions,
    message_tx: &mpsc::Sender<WorkerMessage>,
    elicitation_handler: Arc<ChannelMcpElicitationHandler>,
    mcp_event_handler: Arc<ChannelMcpRuntimeEventHandler>,
    pending_mcp_refreshes: &mut BTreeSet<String>,
) where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let servers = std::mem::take(pending_mcp_refreshes);
    for server_name in servers {
        let Some(agent) = Arc::get_mut(agent) else {
            pending_mcp_refreshes.insert(server_name.clone());
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
        match runtime.block_on(sigil_runtime::refresh_mcp_server_tools_with_mcp_handlers(
            agent.tool_registry_mut(),
            root_config,
            provider_capabilities,
            options.workspace_root.clone(),
            &server_name,
            elicitation_handler_trait,
            mcp_event_handler_trait,
        )) {
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
                    status: McpActivationStatus::Failed {
                        error: error.clone(),
                    },
                });
                let _ = message_tx.send(WorkerMessage::Notice(format!(
                    "MCP refresh failed for {server_name}: {error}"
                )));
            }
        }
    }
}

struct RunTaskResult {
    run_id: u64,
    session: Session,
    result: std::result::Result<AgentRunResult, String>,
}

struct ActiveProviderStatusTask {
    request_id: u64,
    handle: tokio::task::JoinHandle<()>,
}

enum ProviderStatusTaskResult {
    Balance {
        request_id: u64,
        snapshot: BalanceSnapshot,
    },
    Models {
        request_id: u64,
        base_url: String,
        result: std::result::Result<Vec<String>, String>,
    },
}

fn append_mcp_elicitation_audits(
    session: &mut Session,
    audit_buffer: &McpElicitationAuditBuffer,
) -> std::result::Result<(), String> {
    let controls = {
        let mut buffer = audit_buffer
            .lock()
            .map_err(|_| "failed to lock MCP elicitation audit buffer".to_owned())?;
        std::mem::take(&mut *buffer)
    };
    for control in controls {
        session
            .append_control(control)
            .map_err(|error| format!("failed to append MCP elicitation audit: {error:#}"))?;
    }
    Ok(())
}
