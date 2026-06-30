use super::*;

pub(in crate::runner) struct ActiveRun {
    pub(in crate::runner) run_id: u64,
    pub(in crate::runner) handle: tokio::task::JoinHandle<()>,
    pub(in crate::runner) approval_tx: mpsc::Sender<ApprovalSignal>,
    pub(in crate::runner) elicitation_audit_buffer: McpElicitationAuditBuffer,
}

#[allow(clippy::too_many_arguments)]
pub(in crate::runner) fn cancel_active_run(
    active_run: ActiveRun,
    root_config: &RootConfig,
    current_session_log_path: &Path,
    current_session: &mut Option<Session>,
    message_tx: &mpsc::Sender<WorkerMessage>,
    elicitation_handler: &Arc<ChannelMcpElicitationHandler>,
    agent_supervisor: &sigil_runtime::AgentSupervisor,
    discarded_run_ids: &mut BTreeSet<u64>,
    reason: &str,
) {
    elicitation_handler.set_audit_buffer(None);
    discarded_run_ids.insert(active_run.run_id);
    let _ = active_run.approval_tx.send(ApprovalSignal::Cancel);
    let agent_cancel_impact = agent_supervisor.cancel_foreground_run();
    active_run.handle.abort();
    match load_session(
        &root_config.agent.provider,
        &root_config.agent.model,
        current_session_log_path,
    ) {
        Ok(session) => {
            let mut session = session;
            if let Err(error) =
                append_mcp_elicitation_audits(&mut session, &active_run.elicitation_audit_buffer)
            {
                let _ = message_tx.send(WorkerMessage::RunFailed(error));
                *current_session = Some(session);
                return;
            }
            let mut cancel_handler = ChannelEventHandler::new(message_tx.clone());
            if let Err(error) = sigil_runtime::AgentSupervisor::append_foreground_cancel_audit(
                &mut session,
                &mut cancel_handler,
                agent_cancel_impact,
                reason,
            ) {
                let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                    "failed to append cancelled agent state: {error:#}"
                )));
                *current_session = Some(session);
                return;
            }
            if let Err(error) = append_cancelled_task_state(&mut session) {
                let _ = message_tx.send(WorkerMessage::RunFailed(error));
                *current_session = Some(session);
                return;
            }
            let entries = session.entries().to_vec();
            *current_session = Some(session);
            let _ = message_tx.send(WorkerMessage::RunCancelled {
                session_log_path: current_session_log_path.to_path_buf(),
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
}

pub(in crate::runner) struct RunTaskResult {
    pub(in crate::runner) run_id: u64,
    pub(in crate::runner) session: Session,
    pub(in crate::runner) payload: RunTaskPayload,
}

pub(in crate::runner) enum RunTaskPayload {
    Chat {
        result: std::result::Result<AgentRunResult, String>,
        plan_mode: bool,
        queue_id: Option<ConversationInputQueueId>,
        agent_result_continuation_thread_ids: Vec<AgentThreadId>,
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
