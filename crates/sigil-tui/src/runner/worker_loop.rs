use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use sha2::{Digest, Sha256};
use sigil_kernel::{
    Agent, AgentDelegationRequirement, AgentProfileId, AgentRole, AgentRunInput, AgentRunOptions,
    AgentRunResult, AgentThreadId, AgentThreadStatus, ControlEntry, ModelMessage,
    PlanApprovalExpiry, PlanApprovalPermission, PlanApprovalScope, PlanApprovedEntry,
    ProviderCapabilities, RootConfig, SequentialTaskOrchestrator, SequentialTaskRequest, Session,
    SessionLogEntry, SessionRef, SkillDescriptor, SkillRunMode, TaskChildSessionEntry,
    TaskChildSessionStatus, TaskId, TaskRouteId, TaskRouteStatus, TaskRunEntry, TaskRunProjection,
    TaskRunStatus, TaskStepEntry, TaskStepId, TaskStepSpec, TaskStepStatus,
    TaskSubagentElicitationRouteEntry, TerminalTaskEntry, TerminalTaskId, ToolApproval, ToolCall,
    ToolContext, ToolErrorKind, ToolExecutionEntry, ToolExecutionStatus, ToolRegistry, ToolResult,
    ToolResultMeta, ToolResultStatus, ToolSubject, ToolSubjectAudit, default_user_config_dir,
    plan_text_hash, plan_workspace_paths,
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
    workspace_root: PathBuf,
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
    let session_entries = current_session
        .as_ref()
        .map(Session::entries)
        .unwrap_or(&[]);
    let agent_supervisor =
        match sigil_runtime::AgentProfileRegistry::from_root_config_with_workspace_and_entries(
            &root_config,
            &workspace_root,
            session_entries,
        ) {
            Ok(registry) => sigil_runtime::AgentSupervisor::new(
                registry,
                sigil_runtime::AgentBudgetPolicy::from_root_config(&root_config),
                provider_capabilities.clone(),
            ),
            Err(error) => {
                let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                return;
            }
        };
    let background_agent_runs = sigil_runtime::AgentToolBackgroundRuns::default();

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
            match task_result.payload {
                RunTaskPayload::Chat {
                    result: Ok(run_result),
                    plan_mode,
                } => {
                    let entries = current_session
                        .as_ref()
                        .map(|session| session.entries().to_vec())
                        .unwrap_or_default();
                    let message = if plan_mode {
                        WorkerMessage::PlanRunFinished {
                            result: run_result,
                            entries,
                        }
                    } else {
                        WorkerMessage::RunFinished {
                            result: run_result,
                            entries,
                        }
                    };
                    let _ = message_tx.send(message);
                }
                RunTaskPayload::Agent {
                    profile_id,
                    result: Ok(run_result),
                } => {
                    let entries = current_session
                        .as_ref()
                        .map(|session| session.entries().to_vec())
                        .unwrap_or_default();
                    let _ = message_tx.send(WorkerMessage::AgentRunFinished {
                        profile_id,
                        result: run_result,
                        entries,
                    });
                }
                RunTaskPayload::Chat {
                    result: Err(error), ..
                }
                | RunTaskPayload::Agent {
                    result: Err(error), ..
                } => {
                    let _ = message_tx.send(WorkerMessage::RunFailed(error));
                }
                RunTaskPayload::Task {
                    task_id,
                    result: Ok(status),
                } => {
                    let entries = current_session
                        .as_ref()
                        .map(|session| session.entries().to_vec())
                        .unwrap_or_default();
                    let _ = message_tx.send(WorkerMessage::TaskRunFinished {
                        task_id,
                        status,
                        entries,
                    });
                }
                RunTaskPayload::Task {
                    task_id: _,
                    result: Err(error),
                } => {
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

        if active_run.is_none() {
            collect_finished_background_agent_runs(
                &runtime,
                &background_agent_runs,
                &agent_supervisor,
                &root_config,
                agent.tool_registry(),
                &mut current_session,
                &message_tx,
            );
        }

        match command_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(
                command @ (WorkerCommand::SubmitPrompt { .. }
                | WorkerCommand::SubmitPlanPrompt { .. }),
            ) => {
                let (prompt, reasoning_effort, plan_mode) = match command {
                    WorkerCommand::SubmitPrompt {
                        prompt,
                        reasoning_effort,
                    } => (prompt, reasoning_effort, false),
                    WorkerCommand::SubmitPlanPrompt {
                        prompt,
                        reasoning_effort,
                    } => (prompt, reasoning_effort, true),
                    _ => unreachable!("matched submit prompt commands above"),
                };
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

                let started = if plan_mode {
                    WorkerMessage::PlanRunStarted {
                        prompt: prompt.clone(),
                    }
                } else {
                    WorkerMessage::RunStarted {
                        prompt: prompt.clone(),
                    }
                };
                let _ = message_tx.send(started);

                let mut handler = ChannelEventHandler::new(message_tx.clone());
                let (approval_tx, approval_rx) = mpsc::channel();
                let elicitation_audit_buffer: McpElicitationAuditBuffer =
                    Arc::new(std::sync::Mutex::new(Vec::new()));
                elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
                let agent = Arc::clone(&agent);
                let mut options = options.clone();
                options.reasoning_effort = Some(reasoning_effort);
                agent_supervisor.reset_turn_budget();
                let mut agent_delegate = sigil_runtime::AgentToolRuntime::new(
                    agent_supervisor.clone(),
                    root_config.clone(),
                    agent.tool_registry().clone(),
                )
                .with_background_runs(background_agent_runs.clone());
                let plan_tools = plan_mode.then(|| {
                    sigil_runtime::build_plan_prompt_tool_registry(
                        agent.tool_registry(),
                        &root_config,
                    )
                    .into_registry()
                });
                let task_result_tx = task_result_tx.clone();
                let run_id = next_run_id;
                next_run_id += 1;
                let delegation_requirement = agent_delegation_requirement_for_prompt(&prompt);

                let handle = runtime.spawn(async move {
                    let mut run_session = run_session;
                    let result = {
                        let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
                        let mut input = if plan_mode {
                            AgentRunInput::without_persisted_user_message(
                                plan_mode_transient_context(prompt),
                            )
                        } else {
                            AgentRunInput::user(prompt)
                        };
                        if let Some(requirement) = delegation_requirement {
                            input = input.with_agent_delegation_requirement(requirement);
                        }
                        if let Some(tools) = plan_tools {
                            agent
                                .run_with_approval_input_tool_registry_and_agent_delegate(
                                    &mut run_session,
                                    input,
                                    options,
                                    tools,
                                    &mut handler,
                                    &mut approval_handler,
                                    &mut agent_delegate,
                                )
                                .await
                        } else {
                            agent
                                .run_with_approval_input_and_agent_delegate(
                                    &mut run_session,
                                    input,
                                    options,
                                    &mut handler,
                                    &mut approval_handler,
                                    &mut agent_delegate,
                                )
                                .await
                        }
                        .map(|output| output.result)
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
                        payload: RunTaskPayload::Chat { result, plan_mode },
                    });
                });

                active_run = Some(ActiveRun {
                    run_id,
                    handle,
                    approval_tx,
                    elicitation_audit_buffer,
                });
            }
            Ok(WorkerCommand::InvokeAgentProfile { profile_id, prompt }) => {
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

                let _ = message_tx.send(WorkerMessage::AgentRunStarted {
                    profile_id: profile_id.clone(),
                    prompt: prompt.clone(),
                });

                let mut handler = ChannelEventHandler::new(message_tx.clone());
                let (approval_tx, approval_rx) = mpsc::channel();
                let elicitation_audit_buffer: McpElicitationAuditBuffer =
                    Arc::new(std::sync::Mutex::new(Vec::new()));
                elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
                agent_supervisor.reset_turn_budget();
                let mut agent_delegate = sigil_runtime::AgentToolRuntime::new(
                    agent_supervisor.clone(),
                    root_config.clone(),
                    agent.tool_registry().clone(),
                )
                .with_background_runs(background_agent_runs.clone());
                let options = options.clone();
                let task_result_tx = task_result_tx.clone();
                let run_id = next_run_id;
                next_run_id += 1;

                let handle = runtime.spawn(async move {
                    let mut run_session = run_session;
                    let result = {
                        let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
                        match AgentProfileId::new(profile_id.clone()) {
                            Ok(profile_id_value) => agent_delegate
                                .invoke_agent_profile(
                                    &mut run_session,
                                    profile_id_value,
                                    prompt,
                                    &options,
                                    &mut handler,
                                    &mut approval_handler,
                                )
                                .await
                                .map(manual_agent_invocation_result)
                                .map_err(|error| format!("{error:#}")),
                            Err(error) => Err(format!("{error:#}")),
                        }
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
                        payload: RunTaskPayload::Agent { profile_id, result },
                    });
                });

                active_run = Some(ActiveRun {
                    run_id,
                    handle,
                    approval_tx,
                    elicitation_audit_buffer,
                });
            }
            Ok(WorkerCommand::InvokeInlineSkill {
                skill_id,
                arguments,
                reasoning_effort,
            }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "agent is already running".to_owned(),
                    ));
                    continue;
                }

                let run_id = next_run_id;
                let loaded =
                    match load_worker_skill(&root_config, &options, &skill_id, Some(run_id)) {
                        Ok(loaded) => loaded,
                        Err(error) => {
                            let _ = message_tx.send(WorkerMessage::RunFailed(error));
                            continue;
                        }
                    };
                if loaded.descriptor.run_as != SkillRunMode::Inline {
                    let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                        "agent {skill_id} is configured for {} mode, not inline skill mode",
                        loaded.descriptor.run_as.as_str()
                    )));
                    continue;
                }
                let Some(run_session) = current_session.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };

                let prompt = skill_invocation_prompt(&skill_id, &arguments);
                let _ = message_tx.send(WorkerMessage::RunStarted {
                    prompt: prompt.clone(),
                });

                let mut handler = ChannelEventHandler::new(message_tx.clone());
                let (approval_tx, approval_rx) = mpsc::channel();
                let elicitation_audit_buffer: McpElicitationAuditBuffer =
                    Arc::new(std::sync::Mutex::new(Vec::new()));
                elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
                let skill_registry = sigil_runtime::build_skill_tool_registry(
                    agent.tool_registry(),
                    &loaded.descriptor,
                )
                .into_registry();
                let agent = Arc::clone(&agent);
                let mut options = options.clone();
                options.reasoning_effort = Some(reasoning_effort);
                let task_result_tx = task_result_tx.clone();
                next_run_id += 1;

                let handle = runtime.spawn(async move {
                    let mut run_session = run_session;
                    let input = AgentRunInput::transient(prompt, vec![loaded.transient_context]);
                    let result =
                        match run_session.append_control(ControlEntry::SkillLoaded(loaded.entry)) {
                            Ok(()) => {
                                let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
                                agent
                                    .run_with_approval_input_and_tool_registry(
                                        &mut run_session,
                                        input,
                                        options,
                                        skill_registry,
                                        &mut handler,
                                        &mut approval_handler,
                                    )
                                    .await
                                    .map(|output| output.result)
                                    .map_err(|error| format!("{error:#}"))
                            }
                            Err(error) => Err(format!("{error:#}")),
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
                        payload: RunTaskPayload::Chat {
                            result,
                            plan_mode: false,
                        },
                    });
                });

                active_run = Some(ActiveRun {
                    run_id,
                    handle,
                    approval_tx,
                    elicitation_audit_buffer,
                });
            }
            Ok(WorkerCommand::InvokeChildSessionSkill {
                skill_id,
                arguments,
            }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "agent is already running".to_owned(),
                    ));
                    continue;
                }
                if !root_config.task.enabled {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "task planning is disabled in config".to_owned(),
                    ));
                    continue;
                }

                let run_id = next_run_id;
                let loaded =
                    match load_worker_skill(&root_config, &options, &skill_id, Some(run_id)) {
                        Ok(loaded) => loaded,
                        Err(error) => {
                            let _ = message_tx.send(WorkerMessage::RunFailed(error));
                            continue;
                        }
                    };
                if loaded.descriptor.run_as != SkillRunMode::ChildSession {
                    let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                        "skill {skill_id} is configured for {} mode, not agent mode",
                        loaded.descriptor.run_as.as_str()
                    )));
                    continue;
                }
                let Some(run_session) = current_session.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };

                let task_id = match next_task_id(&run_session) {
                    Ok(task_id) => task_id,
                    Err(error) => {
                        current_session = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };
                let task_id_value = task_id.as_str().to_owned();
                let parent_session_ref = match session_ref_for_log_path(&current_session_log_path) {
                    Ok(reference) => reference,
                    Err(error) => {
                        current_session = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };
                let objective = skill_child_session_objective(&skill_id, &arguments);
                let _ = message_tx.send(WorkerMessage::TaskRunStarted {
                    task_id: task_id_value.clone(),
                    objective: objective.clone(),
                });

                let handler = ChannelEventHandler::new(message_tx.clone());
                let (approval_tx, approval_rx) = mpsc::channel();
                let elicitation_audit_buffer: McpElicitationAuditBuffer =
                    Arc::new(std::sync::Mutex::new(Vec::new()));
                elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
                let run_id = next_run_id;
                next_run_id += 1;

                let handle = spawn_skill_child_run(
                    &runtime,
                    SkillChildRunSpawn {
                        run_id,
                        session: run_session,
                        task_id,
                        task_id_value,
                        parent_session_ref,
                        objective,
                        skill_id,
                        arguments,
                        loaded,
                        root_config: root_config.clone(),
                        options: options.clone(),
                        base_registry: agent.tool_registry().clone(),
                        agent_supervisor: agent_supervisor.clone(),
                        task_result_tx: task_result_tx.clone(),
                        approval_rx,
                        handler,
                        elicitation_audit_buffer: run_elicitation_audit_buffer,
                    },
                );

                active_run = Some(ActiveRun {
                    run_id,
                    handle,
                    approval_tx,
                    elicitation_audit_buffer,
                });
            }
            Ok(WorkerCommand::SubmitTask { prompt }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "agent is already running".to_owned(),
                    ));
                    continue;
                }
                if !root_config.task.enabled {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "task planning is disabled in config".to_owned(),
                    ));
                    continue;
                }

                let Some(run_session) = current_session.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };

                let task_id = match next_task_id(&run_session) {
                    Ok(task_id) => task_id,
                    Err(error) => {
                        current_session = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };
                let task_id_value = task_id.as_str().to_owned();
                let parent_session_ref = match session_ref_for_log_path(&current_session_log_path) {
                    Ok(reference) => reference,
                    Err(error) => {
                        current_session = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };
                let _ = message_tx.send(WorkerMessage::TaskRunStarted {
                    task_id: task_id_value.clone(),
                    objective: prompt.clone(),
                });

                let handler = ChannelEventHandler::new(message_tx.clone());
                let (approval_tx, approval_rx) = mpsc::channel();
                let elicitation_audit_buffer: McpElicitationAuditBuffer =
                    Arc::new(std::sync::Mutex::new(Vec::new()));
                elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
                let run_id = next_run_id;
                next_run_id += 1;

                let handle = spawn_task_run(
                    &runtime,
                    TaskRunSpawn {
                        run_id,
                        session: run_session,
                        task_id,
                        task_id_value,
                        parent_session_ref,
                        objective: prompt,
                        root_config: root_config.clone(),
                        options: options.clone(),
                        base_registry: agent.tool_registry().clone(),
                        agent_supervisor: agent_supervisor.clone(),
                        task_result_tx: task_result_tx.clone(),
                        approval_rx,
                        handler,
                        elicitation_audit_buffer: run_elicitation_audit_buffer,
                    },
                );

                active_run = Some(ActiveRun {
                    run_id,
                    handle,
                    approval_tx,
                    elicitation_audit_buffer,
                });
            }
            Ok(WorkerCommand::ContinueTask { task_id, guidance }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "agent is already running".to_owned(),
                    ));
                    continue;
                }
                if !root_config.task.enabled {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "task planning is disabled in config".to_owned(),
                    ));
                    continue;
                }

                let Some(run_session) = current_session.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };
                let (task_id, task_id_value, objective) =
                    match resolve_continue_task(&run_session, task_id) {
                        Ok(resolved) => resolved,
                        Err(error) => {
                            current_session = Some(run_session);
                            let _ = message_tx.send(WorkerMessage::RunFailed(error));
                            continue;
                        }
                    };
                let parent_session_ref = match session_ref_for_log_path(&current_session_log_path) {
                    Ok(reference) => reference,
                    Err(error) => {
                        current_session = Some(run_session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                        continue;
                    }
                };
                let _ = message_tx.send(WorkerMessage::TaskRunStarted {
                    task_id: task_id_value.clone(),
                    objective: objective.clone(),
                });

                let handler = ChannelEventHandler::new(message_tx.clone());
                let (approval_tx, approval_rx) = mpsc::channel();
                let elicitation_audit_buffer: McpElicitationAuditBuffer =
                    Arc::new(std::sync::Mutex::new(Vec::new()));
                elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
                let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
                let run_id = next_run_id;
                next_run_id += 1;

                let handle = spawn_task_continue(
                    &runtime,
                    TaskContinueSpawn {
                        run_id,
                        session: run_session,
                        task_id,
                        task_id_value,
                        parent_session_ref,
                        objective,
                        guidance,
                        root_config: root_config.clone(),
                        options: options.clone(),
                        base_registry: agent.tool_registry().clone(),
                        agent_supervisor: agent_supervisor.clone(),
                        task_result_tx: task_result_tx.clone(),
                        approval_rx,
                        handler,
                        elicitation_audit_buffer: run_elicitation_audit_buffer,
                    },
                );

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
            Ok(WorkerCommand::ApprovalDecisionWithArgs { call_id, args_json }) => {
                if let Some(active_run) = &active_run {
                    let _ = active_run.approval_tx.send(ApprovalSignal::Decision {
                        call_id,
                        approval: ToolApproval::ApproveWithArgs { args_json },
                    });
                } else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "received stray approval decision without pending approval".to_owned(),
                    ));
                }
            }
            Ok(WorkerCommand::BackgroundActiveAgent) => {
                if active_run.is_none() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "no active agent run to background".to_owned(),
                    ));
                    continue;
                }
                match agent_supervisor.request_foreground_background() {
                    Ok(thread_id) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "agent {} background requested",
                            thread_id.as_str()
                        )));
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "agent background unavailable: {error}"
                        )));
                    }
                }
            }
            Ok(WorkerCommand::CancelRun) => {
                if let Some(active_run) = active_run.take() {
                    elicitation_handler.set_audit_buffer(None);
                    discarded_run_ids.insert(active_run.run_id);
                    let _ = active_run.approval_tx.send(ApprovalSignal::Cancel);
                    let agent_cancel_impact = agent_supervisor.cancel_foreground_run();
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
                            let mut cancel_handler = ChannelEventHandler::new(message_tx.clone());
                            if let Err(error) =
                                sigil_runtime::AgentSupervisor::append_foreground_cancel_audit(
                                    &mut session,
                                    &mut cancel_handler,
                                    agent_cancel_impact,
                                    "run cancelled from TUI",
                                )
                            {
                                let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                                    "failed to append cancelled agent state: {error:#}"
                                )));
                                current_session = Some(session);
                                continue;
                            }
                            if let Err(error) = append_cancelled_task_state(&mut session) {
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
            Ok(WorkerCommand::CancelTerminalTask { task_id }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before cancelling terminal task".to_owned(),
                    ));
                    continue;
                }
                match cancel_terminal_task(
                    &runtime,
                    agent.tool_registry().clone(),
                    &root_config,
                    &options,
                    &current_session_log_path,
                    &mut current_session,
                    task_id,
                ) {
                    Ok((entry, entries)) => {
                        let _ =
                            message_tx.send(WorkerMessage::TerminalTaskUpdated { entry, entries });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(error));
                    }
                }
            }
            Ok(WorkerCommand::ApprovePlan {
                plan_text,
                permission,
                scope_summary,
                clear_planning_context,
            }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before approving a plan".to_owned(),
                    ));
                    continue;
                }
                match approve_plan(
                    &root_config,
                    &current_session_log_path,
                    &mut current_session,
                    PlanApprovalRequest {
                        plan_text,
                        permission,
                        scope_summary,
                        clear_planning_context,
                    },
                ) {
                    Ok((entry, entries)) => {
                        let _ = message_tx.send(WorkerMessage::PlanApproved { entry, entries });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(error));
                    }
                }
            }
            Ok(WorkerCommand::CloseAgent { thread_id, reason }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before closing agent".to_owned(),
                    ));
                    continue;
                }
                match close_agent_thread(
                    &root_config,
                    &current_session_log_path,
                    &mut current_session,
                    thread_id,
                    reason,
                ) {
                    Ok((thread_id, entries)) => {
                        let _ = message_tx
                            .send(WorkerMessage::AgentThreadClosed { thread_id, entries });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(error));
                    }
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
            Ok(WorkerCommand::StartNewSession { session_log_path }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "cannot start a new session while the agent is running".to_owned(),
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
                        let _ = message_tx.send(WorkerMessage::NewSessionStarted {
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

struct TaskRunSpawn {
    run_id: u64,
    session: Session,
    task_id: TaskId,
    task_id_value: String,
    parent_session_ref: SessionRef,
    objective: String,
    root_config: RootConfig,
    options: AgentRunOptions,
    base_registry: ToolRegistry,
    agent_supervisor: sigil_runtime::AgentSupervisor,
    task_result_tx: mpsc::Sender<RunTaskResult>,
    approval_rx: mpsc::Receiver<ApprovalSignal>,
    handler: ChannelEventHandler,
    elicitation_audit_buffer: McpElicitationAuditBuffer,
}

struct TaskContinueSpawn {
    run_id: u64,
    session: Session,
    task_id: TaskId,
    task_id_value: String,
    parent_session_ref: SessionRef,
    objective: String,
    guidance: Option<String>,
    root_config: RootConfig,
    options: AgentRunOptions,
    base_registry: ToolRegistry,
    agent_supervisor: sigil_runtime::AgentSupervisor,
    task_result_tx: mpsc::Sender<RunTaskResult>,
    approval_rx: mpsc::Receiver<ApprovalSignal>,
    handler: ChannelEventHandler,
    elicitation_audit_buffer: McpElicitationAuditBuffer,
}

struct SkillChildRunSpawn {
    run_id: u64,
    session: Session,
    task_id: TaskId,
    task_id_value: String,
    parent_session_ref: SessionRef,
    objective: String,
    skill_id: String,
    arguments: String,
    loaded: sigil_runtime::LoadedSkillContext,
    root_config: RootConfig,
    options: AgentRunOptions,
    base_registry: ToolRegistry,
    agent_supervisor: sigil_runtime::AgentSupervisor,
    task_result_tx: mpsc::Sender<RunTaskResult>,
    approval_rx: mpsc::Receiver<ApprovalSignal>,
    handler: ChannelEventHandler,
    elicitation_audit_buffer: McpElicitationAuditBuffer,
}

struct TaskRoleRuntime {
    orchestrator: SequentialTaskOrchestrator<sigil_runtime::AgentSupervisorTaskChildRunner>,
    planner_options: AgentRunOptions,
    executor_options: AgentRunOptions,
    subagent_read_options: AgentRunOptions,
    subagent_write_options: AgentRunOptions,
}

fn spawn_task_run(
    runtime: &tokio::runtime::Runtime,
    spawn: TaskRunSpawn,
) -> tokio::task::JoinHandle<()> {
    runtime.spawn(async move {
        let TaskRunSpawn {
            run_id,
            mut session,
            task_id,
            task_id_value,
            parent_session_ref,
            objective,
            root_config,
            options,
            base_registry,
            agent_supervisor,
            task_result_tx,
            approval_rx,
            mut handler,
            elicitation_audit_buffer,
        } = spawn;
        let result = run_task_orchestration(
            &mut session,
            TaskRunOrchestration {
                task_id,
                parent_session_ref,
                objective,
                root_config,
                options,
                base_registry,
                agent_supervisor,
                approval_rx,
                handler: &mut handler,
            },
        )
        .await;
        let result = match append_mcp_elicitation_audits(&mut session, &elicitation_audit_buffer) {
            Ok(()) => result,
            Err(error) => Err(error),
        };
        send_task_result(run_id, session, task_id_value, result, task_result_tx);
    })
}

fn spawn_task_continue(
    runtime: &tokio::runtime::Runtime,
    spawn: TaskContinueSpawn,
) -> tokio::task::JoinHandle<()> {
    runtime.spawn(async move {
        let TaskContinueSpawn {
            run_id,
            mut session,
            task_id,
            task_id_value,
            parent_session_ref,
            objective,
            guidance,
            root_config,
            options,
            base_registry,
            agent_supervisor,
            task_result_tx,
            approval_rx,
            mut handler,
            elicitation_audit_buffer,
        } = spawn;
        let result = continue_task_orchestration(
            &mut session,
            TaskContinueOrchestration {
                task_id,
                parent_session_ref,
                objective,
                guidance,
                root_config,
                options,
                base_registry,
                agent_supervisor,
                approval_rx,
                handler: &mut handler,
            },
        )
        .await;
        let result = match append_mcp_elicitation_audits(&mut session, &elicitation_audit_buffer) {
            Ok(()) => result,
            Err(error) => Err(error),
        };
        send_task_result(run_id, session, task_id_value, result, task_result_tx);
    })
}

fn spawn_skill_child_run(
    runtime: &tokio::runtime::Runtime,
    spawn: SkillChildRunSpawn,
) -> tokio::task::JoinHandle<()> {
    runtime.spawn(async move {
        let SkillChildRunSpawn {
            run_id,
            mut session,
            task_id,
            task_id_value,
            parent_session_ref,
            objective,
            skill_id,
            arguments,
            loaded,
            root_config,
            options,
            base_registry,
            agent_supervisor,
            task_result_tx,
            approval_rx,
            mut handler,
            elicitation_audit_buffer,
        } = spawn;
        let result = run_skill_child_orchestration(
            &mut session,
            SkillChildRunOrchestration {
                task_id,
                parent_session_ref,
                objective,
                skill_id,
                arguments,
                loaded,
                root_config,
                options,
                base_registry,
                agent_supervisor,
                approval_rx,
                handler: &mut handler,
            },
        )
        .await;
        let result = match append_mcp_elicitation_audits(&mut session, &elicitation_audit_buffer) {
            Ok(()) => result,
            Err(error) => Err(error),
        };
        send_task_result(run_id, session, task_id_value, result, task_result_tx);
    })
}

struct TaskRunOrchestration<'a> {
    task_id: TaskId,
    parent_session_ref: SessionRef,
    objective: String,
    root_config: RootConfig,
    options: AgentRunOptions,
    base_registry: ToolRegistry,
    agent_supervisor: sigil_runtime::AgentSupervisor,
    approval_rx: mpsc::Receiver<ApprovalSignal>,
    handler: &'a mut ChannelEventHandler,
}

struct SkillChildRunOrchestration<'a> {
    task_id: TaskId,
    parent_session_ref: SessionRef,
    objective: String,
    skill_id: String,
    arguments: String,
    loaded: sigil_runtime::LoadedSkillContext,
    root_config: RootConfig,
    options: AgentRunOptions,
    base_registry: ToolRegistry,
    agent_supervisor: sigil_runtime::AgentSupervisor,
    approval_rx: mpsc::Receiver<ApprovalSignal>,
    handler: &'a mut ChannelEventHandler,
}

struct TaskContinueOrchestration<'a> {
    task_id: TaskId,
    parent_session_ref: SessionRef,
    objective: String,
    guidance: Option<String>,
    root_config: RootConfig,
    options: AgentRunOptions,
    base_registry: ToolRegistry,
    agent_supervisor: sigil_runtime::AgentSupervisor,
    approval_rx: mpsc::Receiver<ApprovalSignal>,
    handler: &'a mut ChannelEventHandler,
}

async fn run_task_orchestration(
    session: &mut Session,
    request: TaskRunOrchestration<'_>,
) -> std::result::Result<TaskRunStatus, String> {
    let TaskRunOrchestration {
        task_id,
        parent_session_ref,
        objective,
        root_config,
        options,
        base_registry,
        agent_supervisor,
        approval_rx,
        handler,
    } = request;
    let TaskRoleRuntime {
        orchestrator,
        planner_options,
        executor_options,
        subagent_read_options,
        subagent_write_options,
    } = build_task_role_runtime(&root_config, &options, &base_registry, agent_supervisor)?;
    let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
    orchestrator
        .run(
            session,
            SequentialTaskRequest {
                task_id,
                parent_session_ref,
                objective,
            },
            planner_options,
            executor_options,
            subagent_read_options,
            subagent_write_options,
            root_config.task.max_plan_steps,
            handler,
            &mut approval_handler,
        )
        .await
        .map(|output| output.status)
        .map_err(|error| format!("{error:#}"))
}

async fn continue_task_orchestration(
    session: &mut Session,
    request: TaskContinueOrchestration<'_>,
) -> std::result::Result<TaskRunStatus, String> {
    let TaskContinueOrchestration {
        task_id,
        parent_session_ref,
        objective,
        guidance,
        root_config,
        options,
        base_registry,
        agent_supervisor,
        approval_rx,
        handler,
    } = request;
    let TaskRoleRuntime {
        orchestrator,
        executor_options,
        subagent_read_options,
        subagent_write_options,
        ..
    } = build_task_role_runtime(&root_config, &options, &base_registry, agent_supervisor)?;
    let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
    orchestrator
        .continue_run(
            session,
            SequentialTaskRequest {
                task_id,
                parent_session_ref,
                objective,
            },
            executor_options,
            subagent_read_options,
            subagent_write_options,
            guidance,
            handler,
            &mut approval_handler,
        )
        .await
        .map(|output| output.status)
        .map_err(|error| format!("{error:#}"))
}

async fn run_skill_child_orchestration(
    session: &mut Session,
    request: SkillChildRunOrchestration<'_>,
) -> std::result::Result<TaskRunStatus, String> {
    let SkillChildRunOrchestration {
        task_id,
        parent_session_ref,
        objective,
        skill_id,
        arguments,
        loaded,
        root_config,
        options,
        base_registry,
        agent_supervisor,
        approval_rx,
        handler,
    } = request;
    let child_role = skill_child_agent_role(&loaded.descriptor);
    let TaskRoleRuntime {
        orchestrator,
        subagent_read_options,
        subagent_write_options,
        ..
    } = build_skill_child_role_runtime(
        &root_config,
        &options,
        &base_registry,
        &loaded.descriptor,
        child_role,
        agent_supervisor,
    )?;
    session
        .append_control(ControlEntry::SkillLoaded(loaded.entry))
        .map_err(|error| format!("{error:#}"))?;
    let child_input = AgentRunInput::without_persisted_user_message(vec![
        loaded.transient_context,
        ModelMessage::user(skill_invocation_prompt(&skill_id, &arguments)),
    ]);
    let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
    orchestrator
        .run_direct_child_session(
            session,
            SequentialTaskRequest {
                task_id,
                parent_session_ref,
                objective,
            },
            TaskStepSpec {
                step_id: TaskStepId::new("invoke_skill").map_err(|error| format!("{error:#}"))?,
                title: format!("invoke agent {skill_id}"),
                display_name: Some(skill_id.clone()),
                detail: Some("direct user-invoked agent".to_owned()),
                role: child_role,
            },
            child_input,
            subagent_read_options,
            subagent_write_options,
            handler,
            &mut approval_handler,
        )
        .await
        .map(|output| output.status)
        .map_err(|error| format!("{error:#}"))
}

fn build_task_role_runtime(
    root_config: &RootConfig,
    options: &AgentRunOptions,
    base_registry: &ToolRegistry,
    agent_supervisor: sigil_runtime::AgentSupervisor,
) -> std::result::Result<TaskRoleRuntime, String> {
    agent_supervisor.reset_turn_budget();
    let planner_provider = sigil_runtime::build_role_provider(root_config, AgentRole::Planner)
        .map_err(|error| format!("{error:#}"))?;
    let executor_provider = sigil_runtime::build_role_provider(root_config, AgentRole::Executor)
        .map_err(|error| format!("{error:#}"))?;
    let subagent_read_provider =
        sigil_runtime::build_role_provider(root_config, AgentRole::SubagentRead)
            .map_err(|error| format!("{error:#}"))?;
    let subagent_write_provider =
        sigil_runtime::build_role_provider(root_config, AgentRole::SubagentWrite)
            .map_err(|error| format!("{error:#}"))?;
    let planner_registry =
        sigil_runtime::build_role_tool_registry(base_registry, root_config, AgentRole::Planner)
            .into_registry();
    let executor_registry =
        sigil_runtime::build_role_tool_registry(base_registry, root_config, AgentRole::Executor)
            .into_registry();
    let subagent_read_registry = sigil_runtime::build_role_tool_registry(
        base_registry,
        root_config,
        AgentRole::SubagentRead,
    )
    .into_registry();
    let subagent_write_registry = sigil_runtime::build_role_tool_registry(
        base_registry,
        root_config,
        AgentRole::SubagentWrite,
    )
    .into_registry();
    let workspace_root = options.workspace_root.clone();
    let interaction_mode = options.interaction_mode;
    let child_runner = sigil_runtime::AgentSupervisorTaskChildRunner::new(
        agent_supervisor,
        Agent::new(subagent_read_provider, subagent_read_registry),
        Agent::new(subagent_write_provider, subagent_write_registry),
    );
    Ok(TaskRoleRuntime {
        orchestrator: SequentialTaskOrchestrator::new_with_child_runner(
            Agent::new(planner_provider, planner_registry),
            Agent::new(executor_provider, executor_registry),
            child_runner,
        ),
        planner_options: sigil_runtime::build_role_run_options(
            root_config,
            workspace_root.clone(),
            interaction_mode,
            AgentRole::Planner,
        ),
        executor_options: sigil_runtime::build_role_run_options(
            root_config,
            workspace_root.clone(),
            interaction_mode,
            AgentRole::Executor,
        ),
        subagent_read_options: sigil_runtime::build_role_run_options(
            root_config,
            workspace_root.clone(),
            interaction_mode,
            AgentRole::SubagentRead,
        ),
        subagent_write_options: sigil_runtime::build_role_run_options(
            root_config,
            workspace_root,
            interaction_mode,
            AgentRole::SubagentWrite,
        ),
    })
}

fn build_skill_child_role_runtime(
    root_config: &RootConfig,
    options: &AgentRunOptions,
    base_registry: &ToolRegistry,
    skill: &SkillDescriptor,
    child_role: AgentRole,
    agent_supervisor: sigil_runtime::AgentSupervisor,
) -> std::result::Result<TaskRoleRuntime, String> {
    agent_supervisor.reset_turn_budget();
    let planner_provider = sigil_runtime::build_role_provider(root_config, AgentRole::Planner)
        .map_err(|error| format!("{error:#}"))?;
    let executor_provider = sigil_runtime::build_role_provider(root_config, AgentRole::Executor)
        .map_err(|error| format!("{error:#}"))?;
    let subagent_read_provider =
        sigil_runtime::build_role_provider(root_config, AgentRole::SubagentRead)
            .map_err(|error| format!("{error:#}"))?;
    let subagent_write_provider =
        sigil_runtime::build_role_provider(root_config, AgentRole::SubagentWrite)
            .map_err(|error| format!("{error:#}"))?;
    let planner_registry =
        sigil_runtime::build_role_tool_registry(base_registry, root_config, AgentRole::Planner)
            .into_registry();
    let executor_registry =
        sigil_runtime::build_role_tool_registry(base_registry, root_config, AgentRole::Executor)
            .into_registry();
    let subagent_read_registry = if child_role == AgentRole::SubagentRead {
        sigil_runtime::build_role_skill_tool_registry(
            base_registry,
            root_config,
            AgentRole::SubagentRead,
            skill,
        )
    } else {
        sigil_runtime::build_role_tool_registry(base_registry, root_config, AgentRole::SubagentRead)
    }
    .into_registry();
    let subagent_write_registry = if child_role == AgentRole::SubagentWrite {
        sigil_runtime::build_role_skill_tool_registry(
            base_registry,
            root_config,
            AgentRole::SubagentWrite,
            skill,
        )
    } else {
        sigil_runtime::build_role_tool_registry(
            base_registry,
            root_config,
            AgentRole::SubagentWrite,
        )
    }
    .into_registry();
    let workspace_root = options.workspace_root.clone();
    let interaction_mode = options.interaction_mode;
    let child_runner = sigil_runtime::AgentSupervisorTaskChildRunner::new(
        agent_supervisor,
        Agent::new(subagent_read_provider, subagent_read_registry),
        Agent::new(subagent_write_provider, subagent_write_registry),
    );
    Ok(TaskRoleRuntime {
        orchestrator: SequentialTaskOrchestrator::new_with_child_runner(
            Agent::new(planner_provider, planner_registry),
            Agent::new(executor_provider, executor_registry),
            child_runner,
        ),
        planner_options: sigil_runtime::build_role_run_options(
            root_config,
            workspace_root.clone(),
            interaction_mode,
            AgentRole::Planner,
        ),
        executor_options: sigil_runtime::build_role_run_options(
            root_config,
            workspace_root.clone(),
            interaction_mode,
            AgentRole::Executor,
        ),
        subagent_read_options: sigil_runtime::build_role_run_options(
            root_config,
            workspace_root.clone(),
            interaction_mode,
            AgentRole::SubagentRead,
        ),
        subagent_write_options: sigil_runtime::build_role_run_options(
            root_config,
            workspace_root,
            interaction_mode,
            AgentRole::SubagentWrite,
        ),
    })
}

pub(super) fn skill_child_agent_role(skill: &SkillDescriptor) -> AgentRole {
    let Some(agent) = skill.agent.as_deref() else {
        return AgentRole::SubagentRead;
    };
    match normalized_skill_agent_hint(agent).as_str() {
        "write" | "writer" | "subagentwrite" | "subagentwriter" | "writable" => {
            AgentRole::SubagentWrite
        }
        _ => AgentRole::SubagentRead,
    }
}

fn normalized_skill_agent_hint(agent: &str) -> String {
    agent
        .chars()
        .filter(|value| value.is_ascii_alphanumeric())
        .map(|value| value.to_ascii_lowercase())
        .collect()
}

fn load_worker_skill(
    root_config: &RootConfig,
    options: &AgentRunOptions,
    skill_id: &str,
    run_id: Option<u64>,
) -> std::result::Result<sigil_runtime::LoadedSkillContext, String> {
    let user_config_dir = default_user_config_dir().ok();
    let report = sigil_runtime::discover_skill_index_with_user_dir(
        &options.workspace_root,
        user_config_dir.as_deref(),
        &root_config.skills,
    )
    .map_err(|error| format!("{error:#}"))?;
    sigil_runtime::load_user_invoked_skill(
        &options.workspace_root,
        &report.snapshot,
        skill_id,
        run_id.map(|run_id| run_id.to_string()),
    )
    .map_err(|error| format!("{error:#}"))
}

fn skill_invocation_prompt(skill_id: &str, arguments: &str) -> String {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return format!(
            "Apply the loaded Sigil agent `{skill_id}` to the current task. No additional arguments were provided."
        );
    }
    format!(
        "Apply the loaded Sigil agent `{skill_id}` to the current task with these user-provided arguments:\n\n```text\n{trimmed}\n```"
    )
}

fn skill_child_session_objective(skill_id: &str, arguments: &str) -> String {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return format!("invoke agent {skill_id}");
    }
    format!("invoke agent {skill_id} with arguments: {trimmed}")
}

fn send_task_result(
    run_id: u64,
    session: Session,
    task_id: String,
    result: std::result::Result<TaskRunStatus, String>,
    task_result_tx: mpsc::Sender<RunTaskResult>,
) {
    let _ = task_result_tx.send(RunTaskResult {
        run_id,
        session,
        payload: RunTaskPayload::Task { task_id, result },
    });
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
    payload: RunTaskPayload,
}

enum RunTaskPayload {
    Chat {
        result: std::result::Result<AgentRunResult, String>,
        plan_mode: bool,
    },
    Agent {
        profile_id: String,
        result: std::result::Result<AgentRunResult, String>,
    },
    Task {
        task_id: String,
        result: std::result::Result<TaskRunStatus, String>,
    },
}

fn manual_agent_invocation_result(
    invocation: sigil_runtime::ManualAgentInvocationResult,
) -> AgentRunResult {
    let final_text = invocation
        .result
        .as_ref()
        .map(|result| result.summary.trim())
        .filter(|summary| !summary.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| match invocation.status {
            Some(AgentThreadStatus::Running) | Some(AgentThreadStatus::Started) => format!(
                "agent {} is running in background",
                invocation.thread_id.as_str()
            ),
            Some(AgentThreadStatus::Failed) => {
                format!("agent {} failed", invocation.thread_id.as_str())
            }
            Some(AgentThreadStatus::Cancelled) | Some(AgentThreadStatus::Interrupted) => {
                format!("agent {} was interrupted", invocation.thread_id.as_str())
            }
            _ => format!("agent {} completed", invocation.thread_id.as_str()),
        });
    AgentRunResult {
        final_text,
        tool_calls: 0,
    }
}

fn collect_finished_background_agent_runs(
    runtime: &tokio::runtime::Runtime,
    background_runs: &sigil_runtime::AgentToolBackgroundRuns,
    agent_supervisor: &sigil_runtime::AgentSupervisor,
    root_config: &RootConfig,
    base_registry: &ToolRegistry,
    current_session: &mut Option<Session>,
    message_tx: &mpsc::Sender<WorkerMessage>,
) {
    if !background_runs.has_finished() {
        return;
    }
    let Some(session) = current_session.as_mut() else {
        return;
    };
    let mut handler = ChannelEventHandler::new(message_tx.clone());
    let mut agent_delegate = sigil_runtime::AgentToolRuntime::new(
        agent_supervisor.clone(),
        root_config.clone(),
        base_registry.clone(),
    )
    .with_background_runs(background_runs.clone());
    if let Err(error) =
        runtime.block_on(agent_delegate.collect_finished_background_runs(session, &mut handler))
    {
        let _ = message_tx.send(WorkerMessage::Notice(format!(
            "agent background collection failed: {error:#}"
        )));
    }
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

pub(super) fn append_mcp_elicitation_audits(
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
        if let Some(route) = subagent_elicitation_route_for_control(session, &control) {
            session
                .append_control(route)
                .map_err(|error| format!("failed to append MCP elicitation route: {error:#}"))?;
        }
        session
            .append_control(control)
            .map_err(|error| format!("failed to append MCP elicitation audit: {error:#}"))?;
    }
    Ok(())
}

fn subagent_elicitation_route_for_control(
    session: &Session,
    control: &ControlEntry,
) -> Option<ControlEntry> {
    let ControlEntry::McpElicitation(elicitation) = control else {
        return None;
    };
    let projection = session.task_state_projection();
    let task = projection.latest_task()?;
    let child = current_subagent_child(task)
        .or_else(|| latest_subagent_child_from_entries(session, task))?;
    let status = match elicitation.action {
        sigil_kernel::McpElicitationDecision::Accepted => TaskRouteStatus::Resolved,
        sigil_kernel::McpElicitationDecision::Declined => TaskRouteStatus::Rejected,
        sigil_kernel::McpElicitationDecision::Cancelled => TaskRouteStatus::Cancelled,
    };
    let route_id = TaskRouteId::new(format!(
        "route_mcp_{}",
        stable_route_suffix(&elicitation.message_hash)
    ))
    .ok()?;
    Some(ControlEntry::TaskSubagentElicitationRoute(
        TaskSubagentElicitationRouteEntry {
            route_id,
            task_id: task.task_id.clone(),
            plan_version: child.plan_version,
            step_id: child.step_id.clone(),
            role: child.role,
            child_session_ref: child.child_session_ref.clone(),
            server_name: elicitation.server_name.clone(),
            status,
        },
    ))
}

fn current_subagent_child(task: &TaskRunProjection) -> Option<TaskChildSessionEntry> {
    let (plan_version, step_id) = task.current_step.as_ref()?;
    task.child_sessions
        .values()
        .find(|child| {
            child.plan_version == *plan_version
                && child.step_id == *step_id
                && is_routable_subagent_child(child)
        })
        .cloned()
}

fn latest_subagent_child_from_entries(
    session: &Session,
    task: &TaskRunProjection,
) -> Option<TaskChildSessionEntry> {
    session.entries().iter().rev().find_map(|entry| {
        let SessionLogEntry::Control(ControlEntry::TaskChildSession(child)) = entry else {
            return None;
        };
        if child.task_id == task.task_id && is_routable_subagent_child(child) {
            Some(child.clone())
        } else {
            None
        }
    })
}

fn is_routable_subagent_child(child: &TaskChildSessionEntry) -> bool {
    matches!(
        child.role,
        AgentRole::SubagentRead | AgentRole::SubagentWrite
    ) && child.status != TaskChildSessionStatus::Unavailable
}

fn stable_route_suffix(value: &str) -> String {
    let suffix = value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(16)
        .collect::<String>();
    if suffix.is_empty() {
        "unknown".to_owned()
    } else {
        suffix
    }
}

pub(super) fn cancel_terminal_task(
    runtime: &tokio::runtime::Runtime,
    registry: ToolRegistry,
    root_config: &RootConfig,
    options: &AgentRunOptions,
    current_session_log_path: &Path,
    current_session: &mut Option<Session>,
    task_id: String,
) -> std::result::Result<(TerminalTaskEntry, Vec<SessionLogEntry>), String> {
    let mut session = load_session(
        &root_config.agent.provider,
        &root_config.agent.model,
        current_session_log_path,
    )
    .map_err(|error| format!("failed to load session before terminal cancel: {error:#}"))?;
    let terminal_task_id = TerminalTaskId::new(task_id.clone())
        .map_err(|error| format!("invalid terminal task id: {error:#}"))?;
    let projection = session.terminal_task_projection();
    let previous = projection
        .tasks
        .get(&terminal_task_id)
        .cloned()
        .ok_or_else(|| format!("terminal task {task_id} is not in the current session"))?;
    if !previous.status.is_active() {
        return Err(format!("terminal task {task_id} is not running"));
    }

    let tool_context = ToolContext {
        workspace_root: options.workspace_root.clone(),
        timeout_secs: options.tool_timeout_secs,
    };
    let call = ToolCall {
        id: format!("tui-terminal-cancel-{task_id}"),
        name: "terminal_cancel".to_owned(),
        args_json: serde_json::json!({ "task_id": task_id }).to_string(),
    };
    let subjects = registry
        .permission_subjects(&tool_context, &call)
        .map_err(|error| format!("invalid terminal cancel arguments: {error:#}"))?;
    append_terminal_cancel_execution_audit(
        &mut session,
        &call,
        &subjects,
        ToolExecutionStatus::Started,
        None,
        None,
    )
    .map_err(|error| format!("failed to append terminal cancel audit: {error:#}"))?;

    let execution_started = Instant::now();
    let result = match runtime.block_on(registry.execute(tool_context.clone(), call.clone())) {
        Ok(result) => result,
        Err(error) => ToolResult::error(
            call.id.clone(),
            call.name.clone(),
            ToolErrorKind::Internal,
            format!("terminal cancel failed: {error:#}"),
        ),
    };
    let duration_ms = Some(elapsed_ms(execution_started));
    let execution_status = if result.is_error() {
        ToolExecutionStatus::Failed
    } else {
        ToolExecutionStatus::Completed
    };
    append_terminal_cancel_execution_audit(
        &mut session,
        &call,
        &subjects,
        execution_status,
        duration_ms,
        Some(&result),
    )
    .map_err(|error| format!("failed to append terminal cancel audit: {error:#}"))?;
    if result.is_error() {
        *current_session = Some(session);
        return Err(format!("terminal cancel failed: {}", result.content));
    }
    let entry = terminal_cancel_entry_from_result(&previous, &result)?;
    session
        .append_control(ControlEntry::TerminalTask(entry.clone()))
        .map_err(|error| format!("failed to append terminal task state: {error:#}"))?;
    let entries = session.entries().to_vec();
    *current_session = Some(session);
    Ok((entry, entries))
}

pub(super) fn close_agent_thread(
    root_config: &RootConfig,
    current_session_log_path: &Path,
    current_session: &mut Option<Session>,
    thread_id: AgentThreadId,
    reason: Option<String>,
) -> std::result::Result<(AgentThreadId, Vec<SessionLogEntry>), String> {
    let mut session = load_session(
        &root_config.agent.provider,
        &root_config.agent.model,
        current_session_log_path,
    )
    .map_err(|error| format!("failed to load session before agent close: {error:#}"))?;
    let mut result = sigil_runtime::close_agent_thread(&session, thread_id.clone(), reason);
    if result.is_error() {
        *current_session = Some(session);
        return Err(format!("agent close failed: {}", result.content));
    }

    let mut closed_thread_id = None;
    for control in std::mem::take(&mut result.control_entries) {
        if let ControlEntry::AgentThreadClosed(close) = &control {
            closed_thread_id = Some(close.thread_id.clone());
        }
        session
            .append_control(control)
            .map_err(|error| format!("failed to append agent close state: {error:#}"))?;
    }
    let entries = session.entries().to_vec();
    *current_session = Some(session);
    Ok((closed_thread_id.unwrap_or(thread_id), entries))
}

pub(super) struct PlanApprovalRequest {
    pub(super) plan_text: String,
    pub(super) permission: PlanApprovalPermission,
    pub(super) scope_summary: String,
    pub(super) clear_planning_context: bool,
}

pub(super) fn approve_plan(
    root_config: &RootConfig,
    current_session_log_path: &Path,
    current_session: &mut Option<Session>,
    request: PlanApprovalRequest,
) -> std::result::Result<(PlanApprovedEntry, Vec<SessionLogEntry>), String> {
    let plan_text = request.plan_text.trim();
    if plan_text.is_empty() {
        return Err("plan approval failed: plan text is empty".to_owned());
    }
    let mut session = load_session(
        &root_config.agent.provider,
        &root_config.agent.model,
        current_session_log_path,
    )
    .map_err(|error| format!("failed to load session before plan approval: {error:#}"))?;
    let next_version = session
        .plan_approval_projection()
        .latest_approval
        .as_ref()
        .map(|entry| entry.plan_version.saturating_add(1))
        .unwrap_or(1);
    let scope_summary = if request.scope_summary.trim().is_empty() {
        "approved plan scope".to_owned()
    } else {
        request.scope_summary.trim().to_owned()
    };
    let workspace_paths = plan_workspace_paths(plan_text);
    let entry = PlanApprovedEntry {
        plan_version: next_version,
        plan_hash: plan_text_hash(plan_text),
        approved_at_ms: unix_time_ms(),
        permission: request.permission,
        scope: PlanApprovalScope {
            summary: scope_summary,
            workspace_paths,
        },
        expires: PlanApprovalExpiry::NextUserPrompt,
        clear_planning_context: request.clear_planning_context,
    };
    session
        .append_control(ControlEntry::PlanApproved(entry.clone()))
        .map_err(|error| format!("failed to append plan approval state: {error:#}"))?;
    let entries = session.entries().to_vec();
    *current_session = Some(session);
    Ok((entry, entries))
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn terminal_cancel_entry_from_result(
    previous: &sigil_kernel::TerminalTaskSummary,
    result: &ToolResult,
) -> std::result::Result<TerminalTaskEntry, String> {
    let entry = TerminalTaskEntry::from_tool_result_details(&result.metadata.details)
        .map_err(|error| format!("invalid terminal cancel result: {error:#}"))?
        .ok_or_else(|| "terminal cancel result did not include terminal task state".to_owned())?;
    if entry.handle.task_id != previous.handle.task_id {
        return Err(format!(
            "terminal cancel returned task {}, expected {}",
            entry.handle.task_id.as_str(),
            previous.handle.task_id.as_str()
        ));
    }
    Ok(entry)
}

fn append_terminal_cancel_execution_audit(
    session: &mut Session,
    call: &ToolCall,
    subjects: &[ToolSubject],
    status: ToolExecutionStatus,
    duration_ms: Option<u64>,
    result: Option<&ToolResult>,
) -> anyhow::Result<()> {
    let (changed_files, metadata, error, model_content_hash) = if let Some(result) = result {
        let error = match &result.status {
            ToolResultStatus::Ok => None,
            ToolResultStatus::Error(error) => Some(error.clone()),
        };
        (
            result.metadata.changed_files.clone(),
            result.metadata.clone(),
            error,
            Some(tool_result_model_content_hash(result)),
        )
    } else {
        (
            Vec::new(),
            ToolResultMeta {
                details: serde_json::json!({
                    "call": {
                        "summary": format!("task_id={}", terminal_cancel_task_id_from_call(call))
                    }
                }),
                ..ToolResultMeta::default()
            },
            None,
            None,
        )
    };
    session.append_control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
        call_id: call.id.clone(),
        tool_name: call.name.clone(),
        status,
        duration_ms,
        subjects: subjects.iter().map(ToolSubjectAudit::from).collect(),
        changed_files,
        metadata,
        error,
        model_content_hash,
    })))
}

fn terminal_cancel_task_id_from_call(call: &ToolCall) -> String {
    serde_json::from_str::<serde_json::Value>(&call.args_json)
        .ok()
        .and_then(|value| {
            value
                .get("task_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

fn tool_result_model_content_hash(result: &ToolResult) -> String {
    let mut hasher = Sha256::new();
    hasher.update(result.to_model_content().as_bytes());
    format!("{:x}", hasher.finalize())
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

pub(super) fn append_cancelled_task_state(
    session: &mut Session,
) -> std::result::Result<(), String> {
    let projection = session.task_state_projection();
    let Some(task) = projection.latest_task() else {
        return Ok(());
    };
    if !matches!(task.status, TaskRunStatus::Started | TaskRunStatus::Running) {
        return Ok(());
    }
    let task_id = task.task_id.clone();
    let parent_session_ref = task.parent_session_ref.clone();
    let objective = task.objective.clone();
    let current_step = task.current_step.clone().and_then(|key| {
        task.steps.get(&key).and_then(|step| {
            if step.status.is_terminal() {
                None
            } else {
                Some(step.clone())
            }
        })
    });
    let child_cancellations = task
        .child_sessions
        .values()
        .filter(|child| child.status == TaskChildSessionStatus::Started)
        .cloned()
        .collect::<Vec<_>>();
    let _ = task;

    if let Some(step) = current_step {
        session
            .append_control(ControlEntry::TaskStep(TaskStepEntry {
                task_id: task_id.clone(),
                plan_version: step.plan_version,
                step_id: step.step_id,
                role: step.role,
                status: TaskStepStatus::Cancelled,
                title: step.title,
                summary: None,
                reason: Some("run cancelled from TUI".to_owned()),
            }))
            .map_err(|error| format!("failed to append cancelled task step: {error:#}"))?;
    }
    for mut child in child_cancellations {
        child.status = TaskChildSessionStatus::Cancelled;
        session
            .append_control(ControlEntry::TaskChildSession(child))
            .map_err(|error| format!("failed to append cancelled child session: {error:#}"))?;
    }
    session
        .append_control(ControlEntry::TaskRun(TaskRunEntry {
            task_id,
            parent_session_ref,
            objective,
            status: TaskRunStatus::Cancelled,
            reason: Some("run cancelled from TUI".to_owned()),
        }))
        .map_err(|error| format!("failed to append cancelled task run: {error:#}"))?;
    Ok(())
}

fn session_ref_for_log_path(path: &Path) -> std::result::Result<SessionRef, String> {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("session.jsonl");
    SessionRef::new_relative(file_name)
        .map_err(|error| format!("failed to build parent session ref: {error:#}"))
}

fn plan_mode_transient_context(prompt: String) -> Vec<ModelMessage> {
    vec![
        ModelMessage::system(
            "Plan mode is active for this turn. Research, inspect, and propose a concrete plan, but do not modify files, run write-capable tools, or execute the plan. Use read-only tools and read-only agent delegation when helpful. End with the plan and any open questions needed before implementation.",
        ),
        ModelMessage::user(prompt),
    ]
}

pub(super) fn agent_delegation_requirement_for_prompt(
    prompt: &str,
) -> Option<AgentDelegationRequirement> {
    let normalized = prompt.to_lowercase().replace(
        ['\u{2010}', '\u{2011}', '\u{2012}', '\u{2013}', '\u{2014}'],
        "-",
    );
    let compact = normalized
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    let mentions_subagent = normalized.contains("subagent")
        || normalized.contains("sub-agent")
        || normalized.contains("sub agent")
        || compact.contains("子agent")
        || compact.contains("子代理");
    if !mentions_subagent {
        return None;
    }
    let compact_negations = [
        "不要用子agent",
        "不用子agent",
        "别用子agent",
        "不要使用子agent",
        "不使用子agent",
        "不需要子agent",
        "无需子agent",
        "别开子agent",
        "不要开子agent",
        "不要启动子agent",
    ];
    let normalized_negations = [
        "do not use subagent",
        "do not use sub-agent",
        "don't use subagent",
        "don't use sub-agent",
        "don't use a subagent",
        "don't use a sub-agent",
        "without subagent",
        "without sub-agent",
        "without a subagent",
        "without a sub-agent",
        "do not spawn subagent",
        "do not spawn a sub-agent",
    ];
    let negated = compact_negations
        .iter()
        .any(|phrase| compact.contains(phrase))
        || normalized_negations
            .iter()
            .any(|phrase| normalized.contains(phrase));
    if negated {
        return None;
    }
    let explicit = compact.contains("必须用子agent")
        || compact.contains("必须使用子agent")
        || compact.contains("同时用子agent")
        || compact.contains("让子agent")
        || compact.contains("用子agent")
        || compact.contains("使用子agent")
        || normalized.contains("must use subagent")
        || normalized.contains("must use sub-agent")
        || normalized.contains("use a subagent")
        || normalized.contains("use a sub-agent")
        || normalized.contains("use subagent")
        || normalized.contains("use sub-agent")
        || normalized.contains("delegate to a subagent")
        || normalized.contains("delegate to a sub-agent");
    explicit.then(|| {
        AgentDelegationRequirement::new(
            "the user explicitly requested sub-agent delegation in the current prompt",
        )
    })
}

pub(super) fn next_task_id(session: &Session) -> std::result::Result<TaskId, String> {
    let projection = session.task_state_projection();
    let mut counter = 1usize;
    loop {
        let value = format!("task_{counter}");
        let task_id = TaskId::new(value.clone())
            .map_err(|error| format!("failed to build next task id: {error:#}"))?;
        if !projection.tasks.contains_key(&task_id) {
            return Ok(task_id);
        }
        counter = counter.saturating_add(1);
    }
}

pub(super) fn resolve_continue_task(
    session: &Session,
    requested_task_id: Option<String>,
) -> std::result::Result<(TaskId, String, String), String> {
    let projection = session.task_state_projection();
    let task = match requested_task_id {
        Some(value) => {
            let task_id = TaskId::new(value.clone())
                .map_err(|error| format!("invalid task id for continue: {error:#}"))?;
            projection
                .tasks
                .get(&task_id)
                .ok_or_else(|| format!("task {value} is not present in this session"))?
        }
        None => projection
            .latest_unfinished_task()
            .or_else(|| projection.latest_task())
            .ok_or_else(|| "no task is available to continue".to_owned())?,
    };
    match task.status {
        TaskRunStatus::Completed => {
            return Err(format!(
                "task {} is already completed",
                task.task_id.as_str()
            ));
        }
        TaskRunStatus::Cancelled => {
            return Err(format!("task {} is cancelled", task.task_id.as_str()));
        }
        TaskRunStatus::Started
        | TaskRunStatus::Running
        | TaskRunStatus::Paused
        | TaskRunStatus::Failed
        | TaskRunStatus::Interrupted => {}
    }
    if task.latest_plan_version.is_none() {
        return Err(format!(
            "task {} has no plan to continue",
            task.task_id.as_str()
        ));
    }
    Ok((
        task.task_id.clone(),
        task.task_id.as_str().to_owned(),
        task.objective.clone(),
    ))
}
