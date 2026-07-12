use super::*;

/// Shared owner for detached chat-agent runs that can outlive one parent model turn.
#[derive(Clone, Default)]
pub struct AgentToolBackgroundRuns {
    handles: Arc<Mutex<BTreeMap<AgentThreadId, BackgroundChatAgentHandle>>>,
    event_sink: Option<Arc<dyn AgentToolBackgroundEventSink>>,
}

/// Receives live events emitted by detached child-agent runs.
pub trait AgentToolBackgroundEventSink: Send + Sync {
    fn handle_agent_event(&self, thread_id: &AgentThreadId, event: RunEvent);

    fn handle_agent_status(
        &self,
        _thread_id: &AgentThreadId,
        _status: AgentThreadStatus,
        _reason: Option<String>,
    ) {
    }
}

/// Join handle and durable identity for a detached background chat agent.
pub(super) struct BackgroundChatAgentHandle {
    pub(super) thread: BackgroundChatAgentThreadRecord,
    pub(super) handle: tokio::task::JoinHandle<Result<BackgroundChatAgentResult>>,
    pub(super) cancellation_owner: RunCancellationOwner,
}

pub(super) struct BackgroundCancellationOutcome {
    pub(super) thread: BackgroundChatAgentThreadRecord,
    pub(super) run_scope_id: String,
    pub(super) outcome: RunCancellationTerminalOutcome,
    pub(super) cleanup_complete: bool,
    pub(super) active_effects: usize,
    pub(super) active_tasks: usize,
}

pub(super) struct BackgroundChatAgentThreadRecord {
    pub(super) thread_id: AgentThreadId,
    pub(super) attempt_id: sigil_kernel::AgentRunAttemptId,
    pub(super) profile_id: AgentProfileId,
    pub(super) parent_thread_id: AgentThreadId,
    pub(super) child_session_ref: SessionRef,
    pub(super) budget_scope_id: TaskId,
}

impl BackgroundChatAgentThreadRecord {
    pub(super) fn from_thread(thread: &crate::AgentChatChildThread) -> Self {
        Self {
            thread_id: thread.thread_id.clone(),
            attempt_id: thread.attempt_id.clone(),
            profile_id: thread.profile_id.clone(),
            parent_thread_id: thread.parent_thread_id.clone(),
            child_session_ref: thread.child_session_ref.clone(),
            budget_scope_id: thread.budget_scope_id.clone(),
        }
    }

    pub(super) fn to_runtime_thread(&self) -> crate::AgentChatChildThread {
        crate::AgentChatChildThread {
            thread_id: self.thread_id.clone(),
            attempt_id: self.attempt_id.clone(),
            profile_id: self.profile_id.clone(),
            parent_thread_id: self.parent_thread_id.clone(),
            child_session_ref: self.child_session_ref.clone(),
            budget_scope_id: self.budget_scope_id.clone(),
            mailbox_rx: None,
        }
    }
}

pub(super) struct BackgroundChatAgentResult {
    pub(super) materialized: AgentResultMaterialization,
    pub(super) outcome: AgentRunOutcome,
    pub(super) usage: AgentUsageSummary,
    pub(super) status: TaskChildSessionStatus,
    pub(super) consumed_mailbox_route_ids: Vec<AgentRouteId>,
}

impl AgentToolBackgroundRuns {
    #[must_use]
    pub fn with_event_sink(event_sink: Arc<dyn AgentToolBackgroundEventSink>) -> Self {
        Self {
            handles: Arc::new(Mutex::new(BTreeMap::new())),
            event_sink: Some(event_sink),
        }
    }

    pub(super) fn event_sink(&self) -> Option<Arc<dyn AgentToolBackgroundEventSink>> {
        self.event_sink.clone()
    }

    #[must_use]
    pub fn has_finished(&self) -> bool {
        self.handles
            .lock()
            .map(|handles| {
                handles
                    .values()
                    .any(|background| background.handle.is_finished())
            })
            .unwrap_or(false)
    }

    pub(super) fn insert(
        &self,
        thread_id: AgentThreadId,
        handle: BackgroundChatAgentHandle,
    ) -> Result<()> {
        let mut handles = self
            .handles
            .lock()
            .map_err(|_| anyhow!("agent background run lock poisoned"))?;
        handles.insert(thread_id, handle);
        Ok(())
    }

    pub(super) fn is_running(&self, thread_id: &AgentThreadId) -> bool {
        self.handles
            .lock()
            .map(|handles| {
                handles
                    .get(thread_id)
                    .is_some_and(|background| !background.handle.is_finished())
            })
            .unwrap_or(false)
    }

    pub(super) fn contains(&self, thread_id: &AgentThreadId) -> bool {
        self.handles
            .lock()
            .map(|handles| handles.contains_key(thread_id))
            .unwrap_or(false)
    }

    pub(super) fn remove_if_finished(
        &self,
        thread_id: &AgentThreadId,
    ) -> Option<BackgroundChatAgentHandle> {
        let mut handles = self.handles.lock().ok()?;
        if handles
            .get(thread_id)
            .is_some_and(|background| background.handle.is_finished())
        {
            return handles.remove(thread_id);
        }
        None
    }

    pub(super) fn take_finished(&self) -> Vec<BackgroundChatAgentHandle> {
        let Ok(mut handles) = self.handles.lock() else {
            return Vec::new();
        };
        let finished = handles
            .iter()
            .filter_map(|(thread_id, background)| {
                background.handle.is_finished().then_some(thread_id.clone())
            })
            .collect::<Vec<_>>();
        finished
            .into_iter()
            .filter_map(|thread_id| handles.remove(&thread_id))
            .collect()
    }

    pub(super) fn reserve_cancellation_scope(
        &self,
        thread_id: &AgentThreadId,
    ) -> Result<Option<String>> {
        let handles = self
            .handles
            .lock()
            .map_err(|_| anyhow!("agent background run lock poisoned"))?;
        let Some(background) = handles.get(thread_id) else {
            return Ok(None);
        };
        if !background.cancellation_owner.reserve_cancel() {
            return Ok(None);
        }
        Ok(Some(
            background.cancellation_owner.handle().scope_id().to_owned(),
        ))
    }

    pub(super) async fn cancel(
        &self,
        thread_id: &AgentThreadId,
        timeout: Duration,
    ) -> Result<Option<BackgroundCancellationOutcome>> {
        let Some(mut background) = self
            .handles
            .lock()
            .map_err(|_| anyhow!("agent background run lock poisoned"))?
            .remove(thread_id)
        else {
            return Ok(None);
        };
        let run_scope_id = background.cancellation_owner.handle().scope_id().to_owned();
        let activated = background.cancellation_owner.activate_reserved_cancel();
        debug_assert!(
            activated,
            "reserved background cancellation must activate once"
        );
        let joined = matches!(
            tokio::time::timeout(timeout, &mut background.handle).await,
            Ok(Ok(_))
        );
        let quiescence = if joined {
            background
                .cancellation_owner
                .wait_for_quiescence(Duration::ZERO)
                .await
        } else {
            background.handle.abort();
            let _ = background.handle.await;
            RunQuiescenceOutcome::TimedOut {
                active_effects: background.cancellation_owner.handle().active_effects(),
                active_tasks: background.cancellation_owner.handle().active_tasks(),
            }
        };
        let (outcome, cleanup_complete, active_effects, active_tasks) = match quiescence {
            RunQuiescenceOutcome::Quiescent
                if joined && background.cancellation_owner.cleanup_complete() =>
            {
                (RunCancellationTerminalOutcome::Cancelled, true, 0, 0)
            }
            RunQuiescenceOutcome::Quiescent => {
                (RunCancellationTerminalOutcome::Interrupted, false, 0, 0)
            }
            RunQuiescenceOutcome::TimedOut {
                active_effects,
                active_tasks,
            } => (
                RunCancellationTerminalOutcome::Interrupted,
                false,
                active_effects,
                active_tasks,
            ),
        };
        Ok(Some(BackgroundCancellationOutcome {
            thread: background.thread,
            run_scope_id,
            outcome,
            cleanup_complete,
            active_effects,
            active_tasks,
        }))
    }
}

pub(super) async fn run_background_chat_agent(
    thread_id: AgentThreadId,
    child_agent: Agent<Box<dyn Provider>>,
    mut child_session: Session,
    child_session_ref: SessionRef,
    initial_input: sigil_kernel::AgentRunInput,
    child_options: sigil_kernel::AgentRunOptions,
    mailbox_rx: mpsc::Receiver<AgentMailboxMessage>,
    event_sink: Option<Arc<dyn AgentToolBackgroundEventSink>>,
) -> Result<BackgroundChatAgentResult> {
    let web_task_tree_budget = initial_input.web_task_tree_budget();
    let mut handler = BackgroundChatChildEventHandler {
        thread_id: thread_id.clone(),
        sink: event_sink.clone(),
    };
    let mut approval_handler = BackgroundApprovalHandler;
    let mut latest_output = match child_agent
        .run_with_approval_input(
            &mut child_session,
            initial_input,
            child_options.clone(),
            &mut handler,
            &mut approval_handler,
        )
        .await
    {
        Ok(output) => output,
        Err(error) => {
            emit_background_agent_status(
                event_sink.as_ref(),
                &thread_id,
                AgentThreadStatus::Failed,
                Some(format!("{error:#}")),
            );
            return Err(error);
        }
    };
    let mut consumed_mailbox_route_ids = Vec::new();

    loop {
        let mut prompts = Vec::new();
        while let Ok(message) = mailbox_rx.try_recv() {
            consumed_mailbox_route_ids.push(message.route_id.clone());
            prompts.push(format!(
                "route {}:\n{}",
                message.route_id.as_str(),
                message.prompt.trim()
            ));
        }
        if prompts.is_empty() {
            break;
        }
        let followup_prompt = format!(
            "Parent agent sent follow-up instructions while this child agent was active.\n\n{}",
            prompts.join("\n\n")
        );
        let mut followup_input = sigil_kernel::AgentRunInput::user(followup_prompt);
        if let Some(budget) = web_task_tree_budget.as_ref() {
            followup_input = followup_input.with_web_task_tree_budget(Arc::clone(budget));
        }
        latest_output = match child_agent
            .run_with_approval_input(
                &mut child_session,
                followup_input,
                child_options.clone(),
                &mut handler,
                &mut approval_handler,
            )
            .await
        {
            Ok(output) => output,
            Err(error) => {
                emit_background_agent_status(
                    event_sink.as_ref(),
                    &thread_id,
                    AgentThreadStatus::Failed,
                    Some(format!("{error:#}")),
                );
                return Err(error);
            }
        };
    }

    let materialized = materialize_child_agent_final_answer(
        &mut child_session,
        &child_session_ref,
        &thread_id,
        &latest_output.result,
    )
    .await?;
    let outcome = latest_output.outcome;
    let usage = usage_summary_from_stats(child_session.stats());
    let status = child_status_from_outcome(&materialized.final_text, &outcome);
    emit_background_agent_status(
        event_sink.as_ref(),
        &thread_id,
        agent_status_from_task_child_status(status),
        None,
    );
    Ok(BackgroundChatAgentResult {
        materialized,
        outcome,
        usage,
        status,
        consumed_mailbox_route_ids,
    })
}

struct BackgroundChatChildEventHandler {
    pub(super) thread_id: AgentThreadId,
    sink: Option<Arc<dyn AgentToolBackgroundEventSink>>,
}

impl EventHandler for BackgroundChatChildEventHandler {
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        if let Some(sink) = self.sink.as_ref() {
            sink.handle_agent_event(&self.thread_id, event);
        }
        Ok(())
    }
}

fn emit_background_agent_status(
    sink: Option<&Arc<dyn AgentToolBackgroundEventSink>>,
    thread_id: &AgentThreadId,
    status: AgentThreadStatus,
    reason: Option<String>,
) {
    if let Some(sink) = sink {
        sink.handle_agent_status(thread_id, status, reason);
    }
}

fn agent_status_from_task_child_status(status: TaskChildSessionStatus) -> AgentThreadStatus {
    match status {
        TaskChildSessionStatus::Started => AgentThreadStatus::Started,
        TaskChildSessionStatus::Completed => AgentThreadStatus::Completed,
        TaskChildSessionStatus::Failed => AgentThreadStatus::Failed,
        TaskChildSessionStatus::Cancelled => AgentThreadStatus::Cancelled,
        TaskChildSessionStatus::Interrupted => AgentThreadStatus::Interrupted,
        TaskChildSessionStatus::Unavailable => AgentThreadStatus::Unavailable,
    }
}
