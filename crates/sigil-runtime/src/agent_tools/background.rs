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

    /// Signals that the detached result is visible to [`AgentToolBackgroundRuns`].
    ///
    /// This callback is delivered only after the result slot is published and the run has been
    /// registered. Consumers may therefore collect the named run without polling a `JoinHandle`.
    fn handle_agent_completion_ready(&self, _thread_id: &AgentThreadId) {}
}

/// Join handle and durable identity for a detached background chat agent.
pub(super) struct BackgroundChatAgentHandle {
    pub(super) thread: BackgroundChatAgentThreadRecord,
    pub(super) handle: BackgroundChatAgentTask,
    pub(super) cancellation_owner: RunCancellationOwner,
}

type BackgroundChatAgentOutcome =
    std::result::Result<Result<BackgroundChatAgentResult>, tokio::task::JoinError>;
type BackgroundChatAgentResultSlot = Arc<Mutex<Option<BackgroundChatAgentOutcome>>>;

pub(super) struct BackgroundChatAgentTask {
    join: tokio::task::JoinHandle<()>,
    abort: tokio::task::AbortHandle,
    result: BackgroundChatAgentResultSlot,
    completion_registration: Option<tokio::sync::oneshot::Sender<()>>,
}

impl BackgroundChatAgentTask {
    pub(super) fn spawn<F>(
        thread_id: AgentThreadId,
        event_sink: Option<Arc<dyn AgentToolBackgroundEventSink>>,
        future: F,
    ) -> Self
    where
        F: Future<Output = Result<BackgroundChatAgentResult>> + Send + 'static,
    {
        let result = Arc::new(Mutex::new(None));
        let published_result = Arc::clone(&result);
        let (completion_registration, completion_registration_rx) = if event_sink.is_some() {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            (Some(sender), Some(receiver))
        } else {
            (None, None)
        };
        let inner = tokio::spawn(future);
        let abort = inner.abort_handle();
        let join = tokio::spawn(async move {
            let outcome = inner.await;
            let result_published = if let Ok(mut slot) = published_result.lock() {
                *slot = Some(outcome);
                true
            } else {
                false
            };
            if result_published
                && let Some(sink) = event_sink
                && completion_registration_rx
                    .expect("completion registration exists when an event sink is configured")
                    .await
                    .is_ok()
            {
                sink.handle_agent_completion_ready(&thread_id);
            }
        });
        Self {
            join,
            abort,
            result,
            completion_registration,
        }
    }

    pub(super) fn mark_registered(&mut self) {
        if let Some(registration) = self.completion_registration.take() {
            let _ = registration.send(());
        }
    }

    pub(super) fn is_finished(&self) -> bool {
        self.result
            .lock()
            .map(|result| result.is_some())
            .unwrap_or(false)
    }

    pub(super) fn abort(&self) {
        self.abort.abort();
    }

    pub(super) async fn wait_for_exit(
        &mut self,
    ) -> std::result::Result<(), tokio::task::JoinError> {
        (&mut self.join).await
    }

    pub(super) async fn finish(
        mut self,
    ) -> std::result::Result<Result<BackgroundChatAgentResult>, tokio::task::JoinError> {
        (&mut self.join).await?;
        self.result
            .lock()
            .ok()
            .and_then(|mut result| result.take())
            .unwrap_or_else(|| {
                Ok(Err(anyhow!(
                    "background child agent result slot is unavailable"
                )))
            })
    }
}

/// One join-before-final child owned by the current root run.
///
/// The child task is registered directly against the root cancellation scope. Unlike a detached
/// background run, it has no independent cancellation owner and must be settled before the next
/// parent provider turn.
pub(super) struct JoinedChatAgentHandle {
    pub(super) sequence: u64,
    pub(super) call_id: String,
    pub(super) batch_member: Option<AgentBatchMemberContext>,
    pub(super) thread: BackgroundChatAgentThreadRecord,
    pub(super) future: JoinedChatAgentFuture,
    pub(super) release_guard: ChatChildThreadGuard,
}

#[derive(Clone)]
pub(super) struct AgentBatchMemberContext {
    pub(super) batch_id: String,
    pub(super) request_key: String,
}

pub(super) struct BackgroundCancellationOutcome {
    pub(super) thread: BackgroundChatAgentThreadRecord,
    pub(super) run_scope_id: String,
    pub(super) outcome: RunCancellationTerminalOutcome,
    pub(super) cleanup_complete: bool,
    pub(super) active_effects: usize,
    pub(super) active_tasks: usize,
}

#[derive(Clone)]
pub(super) struct BackgroundChatAgentThreadRecord {
    pub(super) thread_id: AgentThreadId,
    pub(super) attempt_id: sigil_kernel::AgentRunAttemptId,
    pub(super) batch_id: Option<AgentBatchId>,
    pub(super) profile_id: AgentProfileId,
    pub(super) parent_thread_id: AgentThreadId,
    pub(super) child_session_ref: SessionRef,
    pub(super) budget_scope_id: TaskId,
    pub(super) isolation: TaskIsolationMode,
}

impl BackgroundChatAgentThreadRecord {
    pub(super) fn from_thread(thread: &crate::AgentChatChildThread) -> Self {
        Self {
            thread_id: thread.thread_id.clone(),
            attempt_id: thread.attempt_id.clone(),
            batch_id: thread.batch_id.clone(),
            profile_id: thread.profile_id.clone(),
            parent_thread_id: thread.parent_thread_id.clone(),
            child_session_ref: thread.child_session_ref.clone(),
            budget_scope_id: thread.budget_scope_id.clone(),
            isolation: thread.isolation,
        }
    }

    pub(super) fn to_runtime_thread(&self) -> crate::AgentChatChildThread {
        crate::AgentChatChildThread {
            thread_id: self.thread_id.clone(),
            attempt_id: self.attempt_id.clone(),
            batch_id: self.batch_id.clone(),
            profile_id: self.profile_id.clone(),
            parent_thread_id: self.parent_thread_id.clone(),
            child_session_ref: self.child_session_ref.clone(),
            budget_scope_id: self.budget_scope_id.clone(),
            isolation: self.isolation,
            mailbox_rx: None,
        }
    }
}

pub(super) enum BackgroundChatAgentDisposition {
    Finished {
        materialized: AgentResultMaterialization,
        status: TaskChildSessionStatus,
    },
    AwaitingUserInput {
        request: Box<sigil_kernel::PublicUserInputRequestV1>,
    },
}

pub(super) struct BackgroundChatAgentResult {
    pub(super) disposition: BackgroundChatAgentDisposition,
    pub(super) outcome: AgentRunOutcome,
    pub(super) usage: AgentUsageSummary,
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

    /// Returns whether this owner still holds any detached agent run.
    ///
    /// A poisoned lock is treated as occupied so callers that protect session
    /// boundaries fail closed instead of allowing an unobservable run to cross
    /// scopes.
    #[must_use]
    pub fn has_any(&self) -> bool {
        self.handles
            .lock()
            .map(|handles| !handles.is_empty())
            .unwrap_or(true)
    }

    pub(super) fn insert(
        &self,
        thread_id: AgentThreadId,
        mut handle: BackgroundChatAgentHandle,
    ) -> Result<()> {
        let mut handles = self
            .handles
            .lock()
            .map_err(|_| anyhow!("agent background run lock poisoned"))?;
        if handles.contains_key(&thread_id) {
            bail!(
                "agent background run {} is already registered",
                thread_id.as_str()
            );
        }
        handle.handle.mark_registered();
        handles.insert(thread_id, handle);
        Ok(())
    }

    pub(super) fn remove_registration(
        &self,
        thread_id: &AgentThreadId,
    ) -> Option<BackgroundChatAgentHandle> {
        self.handles.lock().ok()?.remove(thread_id)
    }

    /// Atomically registers a detached batch before any member can pass its provider-start gate.
    ///
    /// On error the caller retains every registration and can abort the gated tasks without
    /// dispatching provider work.
    pub(super) fn insert_batch(
        &self,
        registrations: &mut Vec<(AgentThreadId, BackgroundChatAgentHandle)>,
    ) -> Result<()> {
        let mut handles = self
            .handles
            .lock()
            .map_err(|_| anyhow!("agent background run lock poisoned"))?;
        let mut batch_ids = BTreeSet::new();
        for (thread_id, _) in registrations.iter() {
            if !batch_ids.insert(thread_id.clone()) {
                bail!(
                    "agent background batch contains duplicate thread {}",
                    thread_id.as_str()
                );
            }
            if handles.contains_key(thread_id) {
                bail!(
                    "agent background run {} is already registered",
                    thread_id.as_str()
                );
            }
        }
        for (thread_id, mut handle) in registrations.drain(..) {
            handle.handle.mark_registered();
            handles.insert(thread_id, handle);
        }
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
            tokio::time::timeout(timeout, background.handle.wait_for_exit()).await,
            Ok(Ok(()))
        );
        let quiescence = if joined {
            background
                .cancellation_owner
                .wait_for_quiescence(Duration::ZERO)
                .await
        } else {
            background.handle.abort();
            let _ = background.handle.wait_for_exit().await;
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
    thread: BackgroundChatAgentThreadRecord,
    child_agent: Agent<Box<dyn Provider>>,
    mut child_session: Session,
    child_session_ref: SessionRef,
    initial_input: sigil_kernel::AgentRunInput,
    child_options: sigil_kernel::AgentRunOptions,
    mailbox_rx: mpsc::Receiver<AgentMailboxMessage>,
    event_sink: Option<Arc<dyn AgentToolBackgroundEventSink>>,
) -> Result<BackgroundChatAgentResult> {
    let thread_id = thread.thread_id.clone();
    let web_task_tree_budget = initial_input.web_task_tree_budget();
    let tool_artifact_read_budget = initial_input.tool_artifact_read_budget();
    let mut handler = BackgroundChatChildEventHandler {
        thread_id: thread_id.clone(),
        sink: event_sink.clone(),
    };
    let mut approval_handler =
        BackgroundApprovalHandler::new(thread, &child_options.workspace_root)?;
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
            reconcile_failed_background_user_input_continuations(&mut child_session)?;
            emit_background_agent_error_status(event_sink.as_ref(), &thread_id, &error);
            return Err(error);
        }
    };
    let mut consumed_mailbox_route_ids = Vec::new();

    resolve_started_background_user_input_continuations(&mut child_session)?;

    if let Some(result) = background_user_input_result(
        &child_session,
        &latest_output,
        consumed_mailbox_route_ids.clone(),
    )? {
        emit_background_agent_status(
            event_sink.as_ref(),
            &thread_id,
            AgentThreadStatus::Blocked,
            Some("blocked_needs_user_input".to_owned()),
        );
        return Ok(result);
    }

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
        if let Some(budget) = tool_artifact_read_budget.as_ref() {
            followup_input = followup_input.with_tool_artifact_read_budget(budget.clone());
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
                reconcile_failed_background_user_input_continuations(&mut child_session)?;
                emit_background_agent_error_status(event_sink.as_ref(), &thread_id, &error);
                return Err(error);
            }
        };
        resolve_started_background_user_input_continuations(&mut child_session)?;
        if let Some(result) = background_user_input_result(
            &child_session,
            &latest_output,
            consumed_mailbox_route_ids.clone(),
        )? {
            emit_background_agent_status(
                event_sink.as_ref(),
                &thread_id,
                AgentThreadStatus::Blocked,
                Some("blocked_needs_user_input".to_owned()),
            );
            return Ok(result);
        }
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
        disposition: BackgroundChatAgentDisposition::Finished {
            materialized,
            status,
        },
        outcome,
        usage,
        consumed_mailbox_route_ids,
    })
}

fn reconcile_failed_background_user_input_continuations(session: &mut Session) -> Result<()> {
    let pending = session
        .user_input_projection()?
        .pending()
        .filter(|state| state.status == sigil_kernel::UserInputStatusV1::ContinuationStarted)
        .map(|state| {
            (
                state.requested.request.identity.clone(),
                state.requested.request_hash.clone(),
            )
        })
        .collect::<Vec<_>>();
    for (identity, request_hash) in pending {
        sigil_kernel::reconcile_user_input_continuation_after_failed_run(
            session,
            &identity,
            &request_hash,
            unix_time_ms(),
        )?;
    }
    Ok(())
}

fn resolve_started_background_user_input_continuations(session: &mut Session) -> Result<()> {
    let pending = session
        .user_input_projection()?
        .pending()
        .filter(|state| state.status == sigil_kernel::UserInputStatusV1::ContinuationStarted)
        .map(|state| {
            sigil_kernel::UserInputLifecycleEntryV1::Resolved(sigil_kernel::UserInputResolvedV1 {
                schema_version: sigil_kernel::USER_INPUT_SCHEMA_VERSION,
                identity: state.requested.request.identity.clone(),
                request_hash: state.requested.request_hash.clone(),
                resolution: sigil_kernel::UserInputResolutionV1::Consumed,
                resolved_at_unix_ms: unix_time_ms(),
            })
        })
        .collect::<Vec<_>>();
    if !pending.is_empty() {
        session.append_user_input_lifecycle(pending)?;
    }
    Ok(())
}

fn background_user_input_result(
    child_session: &Session,
    output: &sigil_kernel::AgentRunOutput,
    consumed_mailbox_route_ids: Vec<AgentRouteId>,
) -> Result<Option<BackgroundChatAgentResult>> {
    let sigil_kernel::AgentRunDisposition::AwaitingUserInput(reference) = &output.disposition
    else {
        return Ok(None);
    };
    let projection = child_session.user_input_projection()?;
    let request = projection
        .request(&reference.identity)
        .filter(|state| state.requested.request_hash == reference.request_hash)
        .map(sigil_kernel::UserInputRequestStateV1::public_view)
        .ok_or_else(|| {
            anyhow!("background child suspended without its durable user-input request")
        })?;
    Ok(Some(BackgroundChatAgentResult {
        disposition: BackgroundChatAgentDisposition::AwaitingUserInput {
            request: Box::new(request),
        },
        outcome: output.outcome.clone(),
        usage: usage_summary_from_stats(child_session.stats()),
        consumed_mailbox_route_ids,
    }))
}

struct BackgroundChatChildEventHandler {
    pub(super) thread_id: AgentThreadId,
    sink: Option<Arc<dyn AgentToolBackgroundEventSink>>,
}

impl EventHandler for BackgroundChatChildEventHandler {
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        if let Some(sink) = self.sink.as_ref() {
            if matches!(event, RunEvent::ToolApprovalRequested { .. }) {
                sink.handle_agent_event(
                    &self.thread_id,
                    RunEvent::Notice(format!(
                        "agent {} paused because a tool requires approval; inspect the agent route to resume with a fresh preview",
                        self.thread_id.as_str()
                    )),
                );
            } else {
                sink.handle_agent_event(&self.thread_id, event);
            }
        }
        Ok(())
    }
}

fn emit_background_agent_error_status(
    sink: Option<&Arc<dyn AgentToolBackgroundEventSink>>,
    thread_id: &AgentThreadId,
    error: &anyhow::Error,
) {
    let (status, reason) = if let Some(blocked) = error.downcast_ref::<BackgroundApprovalRequired>()
    {
        (
            AgentThreadStatus::Blocked,
            format!(
                "blocked_needs_approval:{}",
                blocked.route().route_id.as_str()
            ),
        )
    } else {
        (AgentThreadStatus::Failed, format!("{error:#}"))
    };
    emit_background_agent_status(sink, thread_id, status, Some(reason));
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
