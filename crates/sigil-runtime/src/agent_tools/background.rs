use super::*;

/// Shared owner for detached chat-agent runs that can outlive one parent model turn.
#[derive(Clone, Default)]
pub struct AgentToolBackgroundRuns {
    handles: Arc<Mutex<BTreeMap<AgentThreadId, BackgroundChatAgentHandle>>>,
    event_sink: Arc<Mutex<Option<Arc<dyn AgentToolBackgroundEventSink>>>>,
}

impl std::fmt::Debug for AgentToolBackgroundRuns {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (registered_threads, event_sink_registered) = self
            .handles
            .lock()
            .map(|handles| {
                let event_sink_registered = self
                    .event_sink
                    .lock()
                    .map(|sink| sink.is_some())
                    .unwrap_or(false);
                (handles.len(), event_sink_registered)
            })
            .unwrap_or((usize::MAX, false));
        formatter
            .debug_struct("AgentToolBackgroundRuns")
            .field("registered_threads", &registered_threads)
            .field("event_sink_registered", &event_sink_registered)
            .finish()
    }
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
    /// Shared lifecycle state from the exact admitting supervisor, detached from its owner map
    /// to avoid a strong-reference cycle through `AgentToolBackgroundRuns`.
    pub(super) collection_supervisor: AgentSupervisor,
    pub(super) cancellation_owner: RunCancellationOwner,
    pub(super) write_owner: Option<BackgroundChatAgentWriteOwner>,
}

/// Process-local owner for an isolated write result until its durable merge proposal is recorded.
pub(super) enum BackgroundChatAgentWriteOwner {
    ChangesetOnly {
        source_observation: ChangesetSourceObservation,
        workspace_root: PathBuf,
    },
    Worktree {
        worktree: Box<crate::isolated_workspace::MaterializedGitWorktree>,
        workspace_root: PathBuf,
        objective: String,
    },
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
    pub(super) collection_supervisor: AgentSupervisor,
    pub(super) cancellation_owner: RunCancellationOwner,
    pub(super) write_owner: Option<BackgroundChatAgentWriteOwner>,
    pub(super) run_scope_id: String,
    pub(super) outcome: RunCancellationTerminalOutcome,
    pub(super) cleanup_complete: bool,
    pub(super) active_effects: usize,
    pub(super) active_tasks: usize,
}

/// Durable observation of cancelling one process-owned background child.
#[derive(Debug, Clone)]
pub(crate) struct BackgroundAgentCancellation {
    pub(crate) previous_status: AgentThreadStatus,
    pub(crate) status: AgentThreadStatus,
    pub(crate) status_label: &'static str,
    pub(crate) reason: String,
    pub(crate) outcome: RunCancellationTerminalOutcome,
    pub(crate) cleanup_complete: bool,
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

/// Promotes queued background follow-ups through the kernel's active conversation invocation.
///
/// The receiver is shared behind an async mutex because the kernel owns the provider trait for
/// the lifetime of one run. Route ids are retained separately so the supervisor can append their
/// durable consumed entries after the same invocation reaches a terminal disposition.
struct BackgroundMailboxPendingInputProvider {
    mailbox_rx: tokio::sync::Mutex<mpsc::Receiver<AgentMailboxMessage>>,
    consumed_route_ids: Arc<tokio::sync::Mutex<Vec<AgentRouteId>>>,
}

#[async_trait]
impl sigil_kernel::PendingConversationInputProvider for BackgroundMailboxPendingInputProvider {
    async fn promote_next_pending_input(
        &self,
        session: &mut Session,
        logical_run_id: &str,
    ) -> Result<Option<sigil_kernel::PromotedConversationInput>> {
        let mailbox = self.mailbox_rx.lock().await;
        let mut messages = Vec::new();
        while let Ok(message) = mailbox.try_recv() {
            self.consumed_route_ids
                .lock()
                .await
                .push(message.route_id.clone());
            messages.push(message);
        }
        drop(mailbox);
        if messages.is_empty() {
            return Ok(None);
        }
        let prompt = messages
            .iter()
            .map(|message| {
                format!(
                    "route {}:\n{}",
                    message.route_id.as_str(),
                    message.prompt.trim()
                )
            })
            .collect::<Vec<_>>();
        let prompt = format!(
            "Parent agent sent follow-up instructions while this child agent was active (run {logical_run_id}).\n\n{}",
            prompt.join("\n\n")
        );
        let prompt = sigil_kernel::safe_persistence_text(&prompt);
        session.append_user_message(sigil_kernel::ModelMessage::user(prompt.clone()))?;
        Ok(Some(sigil_kernel::PromotedConversationInput {
            prompt,
            runtime_context: Default::default(),
        }))
    }
}

impl AgentToolBackgroundRuns {
    #[must_use]
    pub fn with_event_sink(event_sink: Arc<dyn AgentToolBackgroundEventSink>) -> Self {
        Self {
            handles: Arc::new(Mutex::new(BTreeMap::new())),
            event_sink: Arc::new(Mutex::new(Some(event_sink))),
        }
    }

    pub(super) fn event_sink(&self) -> Option<Arc<dyn AgentToolBackgroundEventSink>> {
        self.event_sink.lock().ok().and_then(|sink| sink.clone())
    }

    /// Rebinds live notifications when the session attachment is resumed by a new surface worker.
    /// The background task owner itself remains the same across the rebind.
    pub fn set_event_sink(&self, event_sink: Arc<dyn AgentToolBackgroundEventSink>) -> Result<()> {
        *self
            .event_sink
            .lock()
            .map_err(|_| anyhow!("agent background event sink lock poisoned"))? = Some(event_sink);
        Ok(())
    }

    /// Returns the exact process-local run owner identity shared by two runtime handles.
    #[must_use]
    pub fn shares_owner_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.handles, &other.handles)
    }

    /// Returns every child invocation whose result is still owned by this attachment.
    pub fn thread_ids(&self) -> Result<BTreeSet<AgentThreadId>> {
        self.handles
            .lock()
            .map(|handles| handles.keys().cloned().collect())
            .map_err(|_| anyhow!("agent background run lock poisoned"))
    }

    /// Returns durable isolated-workspace IDs that must stay alive with this attachment.
    pub fn active_worktree_ids(&self) -> Result<BTreeSet<String>> {
        let handles = self
            .handles
            .lock()
            .map_err(|_| anyhow!("agent background run lock poisoned"))?;
        Ok(handles
            .values()
            .filter_map(|background| match background.write_owner.as_ref() {
                Some(BackgroundChatAgentWriteOwner::Worktree { worktree, .. }) => {
                    Some(worktree.isolated_workspace_id().to_owned())
                }
                Some(BackgroundChatAgentWriteOwner::ChangesetOnly { .. }) | None => None,
            })
            .collect())
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
        let collection_supervisor = background.collection_supervisor.clone();
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
            collection_supervisor,
            cancellation_owner: background.cancellation_owner,
            write_owner: background.write_owner,
            run_scope_id,
            outcome,
            cleanup_complete,
            active_effects,
            active_tasks,
        }))
    }

    /// Cancels one process-owned child and durably records the observed cleanup result.
    ///
    /// This is shared by the model-facing `cancel_agent` tool and application-level Task
    /// cancellation so all surfaces use the same owner, audit records, and terminal controls.
    pub(crate) async fn cancel_agent_thread_durably(
        &self,
        session: &mut Session,
        thread_id: &AgentThreadId,
        reason: String,
        handler: &mut (dyn EventHandler + Send),
    ) -> Result<Option<BackgroundAgentCancellation>> {
        const QUIESCENCE_TIMEOUT: Duration = Duration::from_secs(5);

        let projection = session.agent_thread_state_projection();
        let Some(thread) = projection.threads.get(thread_id) else {
            bail!("agent thread {} was not found", thread_id.as_str());
        };
        let previous_status = thread.status;
        if previous_status.is_terminal() {
            bail!(
                "agent thread {} is already {}",
                thread_id.as_str(),
                thread_status_label(previous_status)
            );
        }

        let recorder = session.run_cancellation_recorder()?;
        let Some(run_scope_id) = self.reserve_cancellation_scope(thread_id)? else {
            return Ok(None);
        };
        let request_id = format!("cancel-{run_scope_id}");
        let requested_at_ms = unix_time_ms();
        let request = RunCancellationRequestedEntry {
            request_id: request_id.clone(),
            run_scope_id: run_scope_id.clone(),
            target: RunCancellationTarget::AgentThread {
                thread_id: thread_id.as_str().to_owned(),
            },
            reason: reason.clone(),
            requested_at_ms,
            quiescence_deadline_ms: requested_at_ms
                .saturating_add(QUIESCENCE_TIMEOUT.as_millis() as u64),
        };
        if let Err(error) = recorder.append_requested(&request) {
            if let Some(mut cancellation) = self.cancel(thread_id, QUIESCENCE_TIMEOUT).await? {
                cancellation
                    .collection_supervisor
                    .release_runtime_thread(thread_id);
                if cancellation.outcome == RunCancellationTerminalOutcome::Cancelled
                    && cancellation.cleanup_complete
                    && let Some(write_owner) = cancellation.write_owner.take()
                {
                    super::chat::cleanup_background_isolated_write_owner(
                        session,
                        handler,
                        write_owner,
                    )
                    .await
                    .context(
                        "failed to clean up the isolated workspace after cancellation audit failure",
                    )?;
                }
            }
            return Err(error.context("failed to persist agent cancellation request"));
        }

        let Some(mut cancellation) = self.cancel(thread_id, QUIESCENCE_TIMEOUT).await? else {
            bail!(
                "agent thread {} lost its runtime owner during cancellation",
                thread_id.as_str()
            );
        };
        let write_cleanup_error = if cancellation.outcome
            == RunCancellationTerminalOutcome::Cancelled
            && cancellation.cleanup_complete
            && let Some(write_owner) = cancellation.write_owner.take()
        {
            super::chat::cleanup_background_isolated_write_owner(session, handler, write_owner)
                .await
                .err()
        } else {
            None
        };
        let route_revocation_error = if cancellation.outcome
            == RunCancellationTerminalOutcome::Cancelled
            && cancellation.cleanup_complete
        {
            self.revoke_child_pending_routes(session, thread_id, handler)
                .await
                .err()
        } else {
            None
        };
        if write_cleanup_error.is_some() || route_revocation_error.is_some() {
            cancellation
                .cancellation_owner
                .handle()
                .mark_cleanup_incomplete();
            cancellation.cleanup_complete = false;
            cancellation.outcome = RunCancellationTerminalOutcome::Interrupted;
        }
        cancellation
            .collection_supervisor
            .release_runtime_thread(thread_id);
        let (status, status_label, terminal_reason) = match cancellation.outcome {
            RunCancellationTerminalOutcome::Cancelled => {
                (AgentThreadStatus::Cancelled, "cancelled", reason)
            }
            RunCancellationTerminalOutcome::Interrupted => (
                AgentThreadStatus::Interrupted,
                "interrupted",
                write_cleanup_error.map_or_else(
                    || {
                        route_revocation_error.map_or_else(
                            || {
                                "cancellation deadline exceeded; cleanup could not be confirmed"
                                    .to_owned()
                            },
                            |error| format!("background child route revocation failed: {error:#}"),
                        )
                    },
                    |error| format!("background isolated-workspace cleanup failed: {error:#}"),
                ),
            ),
        };
        recorder.append_finalized(&RunCancellationFinalizedEntry {
            request_id,
            run_scope_id: cancellation.run_scope_id,
            outcome: cancellation.outcome,
            cleanup_complete: cancellation.cleanup_complete,
            active_effects: cancellation.active_effects,
            active_tasks: cancellation.active_tasks,
            reason: terminal_reason.clone(),
            finalized_at_ms: unix_time_ms(),
        })?;
        let controls = [
            ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
                thread_id: thread_id.clone(),
                status,
                reason: Some(terminal_reason.clone()),
                updated_at_ms: Some(unix_time_ms()),
            }),
            ControlEntry::AgentRunInterrupted(AgentRunInterruptedEntry {
                thread_id: thread_id.clone(),
                attempt_id: cancellation.thread.attempt_id,
                reason: terminal_reason.clone(),
            }),
        ];
        for control in controls {
            handler.commit_controls(session, vec![control])?;
        }

        Ok(Some(BackgroundAgentCancellation {
            previous_status,
            status,
            status_label,
            reason: terminal_reason,
            outcome: cancellation.outcome,
            cleanup_complete: cancellation.cleanup_complete,
        }))
    }

    async fn revoke_child_pending_routes(
        &self,
        session: &mut Session,
        thread_id: &AgentThreadId,
        handler: &mut (dyn EventHandler + Send),
    ) -> Result<(bool, Option<sigil_kernel::AgentRunAttemptId>)> {
        let projection = session.agent_thread_state_projection();
        let input_routes =
            sigil_kernel::AgentUserInputRouteProjectionV1::from_session_entries(session.entries())?
                .routes_for_thread(thread_id)
                .filter(|route| {
                    matches!(
                        route.status,
                        AgentRouteStatus::Requested | AgentRouteStatus::Registered
                    )
                })
                .cloned()
                .collect::<Vec<_>>();
        let mut approval_routes = projection
            .approval_routes
            .values()
            .filter(|route| {
                &route.source_thread_id == thread_id
                    && matches!(
                        route.status,
                        AgentRouteStatus::Requested | AgentRouteStatus::Registered
                    )
            })
            .cloned()
            .collect::<Vec<_>>();
        if input_routes.is_empty() && approval_routes.is_empty() {
            return Ok((false, None));
        }

        let mut controls = Vec::new();
        let mut interrupted_attempt = None;
        for route in input_routes {
            let mut child =
                super::shared::build_agent_child_session(session, &route.child_session_ref)?;
            let command_id = sigil_kernel::UserInputCommandId::new(format!(
                "cancel-{}",
                short_digest(&hash_text(&format!(
                    "{}:{}:{}",
                    thread_id.as_str(),
                    route.route_id.as_str(),
                    route.request.request_hash,
                )))
            ))?;
            let command = sigil_kernel::UserInputDecisionCommandV1 {
                identity: route.request.identity.clone(),
                request_hash: route.request.request_hash.clone(),
                command_id,
                decision: sigil_kernel::UserInputDecisionV1::RunCancelled,
            };
            let now = unix_time_ms();
            sigil_kernel::preview_user_input_decision(&child, &command, now)?;
            let receipt = sigil_kernel::accept_user_input_decision(&mut child, command, now)?;
            let mut closed_route = route.clone();
            closed_route.request = receipt.request;
            closed_route.status = AgentRouteStatus::Cancelled;
            closed_route.updated_at_unix_ms = unix_time_ms();
            interrupted_attempt.get_or_insert(route.source_attempt_id);
            controls.push(ControlEntry::AgentUserInputRoute(closed_route));
        }
        for mut route in approval_routes.drain(..) {
            if interrupted_attempt.is_none() {
                interrupted_attempt = route
                    .binding
                    .as_ref()
                    .map(|binding| binding.attempt_id.clone());
            }
            route.status = AgentRouteStatus::Cancelled;
            controls.push(ControlEntry::AgentApprovalRoute(route));
        }
        handler.commit_controls(session, controls)?;
        Ok((true, interrupted_attempt))
    }

    /// Cancels every live child durably owned by one exact Direct Task.
    ///
    /// The parent Task may already have recorded `Interrupted` or `Paused` when its root run
    /// finalizer observes cancellation. Child cleanup is still required, so this operation is
    /// intentionally selected by the durable cancellation-scope binding rather than by the
    /// Task's current status.
    ///
    /// # Errors
    ///
    /// Returns an error when the exact Task's live children cannot all be cancelled and cleaned
    /// up durably. Returns `Ok(None)` when the cancellation scope is not bound to a Task.
    pub async fn cancel_task_background_agents_for_scope(
        &self,
        session: &mut Session,
        cancellation_target: &RunCancellationTarget,
        run_scope_id: &str,
        reason: &str,
        handler: &mut (dyn EventHandler + Send),
    ) -> Result<Option<sigil_kernel::TaskId>> {
        let Some(task_id) = crate::agent_supervisor::task_execution::task_id_for_cancellation_scope(
            session.entries(),
            cancellation_target,
            run_scope_id,
        ) else {
            return Ok(None);
        };
        self.cancel_direct_task_agents_durably(session, &task_id, reason, handler)
            .await?;
        Ok(Some(task_id))
    }

    pub(crate) async fn cancel_direct_task_agents_durably(
        &self,
        session: &mut Session,
        task_id: &sigil_kernel::TaskId,
        reason: &str,
        handler: &mut (dyn EventHandler + Send),
    ) -> Result<()> {
        let thread_ids = session
            .task_state_projection()
            .direct_task_background_agents(task_id);
        let mut failures = Vec::new();
        for thread_id in thread_ids {
            let projection = session.agent_thread_state_projection();
            if projection
                .threads
                .get(&thread_id)
                .is_some_and(|thread| thread.status.is_terminal())
            {
                if projection
                    .threads
                    .get(&thread_id)
                    .is_some_and(|thread| thread.status == AgentThreadStatus::Interrupted)
                    && let Err(error) = self
                        .revoke_child_pending_routes(session, &thread_id, handler)
                        .await
                {
                    failures.push(format!(
                        "interrupted agent {} route revocation failed: {error:#}",
                        thread_id.as_str()
                    ));
                }
                continue;
            }
            match self
                .cancel_agent_thread_durably(session, &thread_id, reason.to_owned(), handler)
                .await
            {
                Ok(Some(cancellation))
                    if cancellation.outcome == RunCancellationTerminalOutcome::Cancelled
                        && cancellation.cleanup_complete => {}
                Ok(Some(cancellation)) => failures.push(format!(
                    "agent {} cleanup was not confirmed ({})",
                    thread_id.as_str(),
                    cancellation.status_label
                )),
                Ok(None)
                    if projection
                        .threads
                        .get(&thread_id)
                        .is_some_and(|thread| thread.status == AgentThreadStatus::Blocked) =>
                {
                    match self
                        .revoke_child_pending_routes(session, &thread_id, handler)
                        .await
                    {
                        Ok((true, attempt_id)) => {
                            let mut controls = vec![ControlEntry::AgentThreadStatusChanged(
                                AgentThreadStatusChangedEntry {
                                    thread_id: thread_id.clone(),
                                    status: AgentThreadStatus::Cancelled,
                                    reason: Some(reason.to_owned()),
                                    updated_at_ms: Some(unix_time_ms()),
                                },
                            )];
                            if let Some(attempt_id) = attempt_id {
                                controls.push(ControlEntry::AgentRunInterrupted(
                                    AgentRunInterruptedEntry {
                                        thread_id: thread_id.clone(),
                                        attempt_id,
                                        reason: reason.to_owned(),
                                    },
                                ));
                            }
                            handler.commit_controls(session, controls)?;
                        }
                        Ok((false, _)) => failures.push(format!(
                            "blocked agent {} has no revocable pending route",
                            thread_id.as_str()
                        )),
                        Err(error) => failures.push(format!(
                            "blocked agent {} route revocation failed: {error:#}",
                            thread_id.as_str()
                        )),
                    }
                }
                Ok(None) => failures.push(format!(
                    "agent {} has no process-local cancellation owner",
                    thread_id.as_str()
                )),
                Err(error) => failures.push(format!(
                    "agent {} cancellation failed: {error:#}",
                    thread_id.as_str()
                )),
            }
        }
        if !failures.is_empty() {
            bail!(
                "Task {} background cleanup could not be confirmed: {}",
                task_id.as_str(),
                failures.join("; ")
            );
        }
        Ok(())
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
    isolated_write: bool,
    event_sink: Option<Arc<dyn AgentToolBackgroundEventSink>>,
) -> Result<BackgroundChatAgentResult> {
    let thread_id = thread.thread_id.clone();
    let consumed_route_ids = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let pending_input_provider = Arc::new(BackgroundMailboxPendingInputProvider {
        mailbox_rx: tokio::sync::Mutex::new(mailbox_rx),
        consumed_route_ids: Arc::clone(&consumed_route_ids),
    });
    let initial_input = initial_input.with_pending_input_provider(pending_input_provider);
    let mut handler = BackgroundChatChildEventHandler {
        thread_id: thread_id.clone(),
        sink: event_sink.clone(),
    };
    let mut approval_handler =
        BackgroundApprovalHandler::new(thread, &child_options.workspace_root)?;
    let latest_output = match child_agent
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
    resolve_started_background_user_input_continuations(&mut child_session)?;

    if let Some(result) = background_user_input_result(
        &child_session,
        &latest_output,
        consumed_route_ids.lock().await.clone(),
    )? {
        if isolated_write {
            reconcile_failed_background_user_input_continuations(&mut child_session)?;
            bail!("background isolated-write child requested user input; resume it in foreground");
        }
        emit_background_agent_status(
            event_sink.as_ref(),
            &thread_id,
            AgentThreadStatus::Blocked,
            Some("blocked_needs_user_input".to_owned()),
        );
        return Ok(result);
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
        consumed_mailbox_route_ids: consumed_route_ids.lock().await.clone(),
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
        let status = match sigil_kernel::agent::execution::execution_failure_disposition(error) {
            sigil_kernel::agent::execution::ExecutionDisposition::Blocked => {
                AgentThreadStatus::Blocked
            }
            sigil_kernel::agent::execution::ExecutionDisposition::Cancelled => {
                AgentThreadStatus::Cancelled
            }
            sigil_kernel::agent::execution::ExecutionDisposition::Interrupted => {
                AgentThreadStatus::Interrupted
            }
            _ => AgentThreadStatus::Failed,
        };
        (status, format!("{error:#}"))
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
        TaskChildSessionStatus::Blocked => AgentThreadStatus::Blocked,
        TaskChildSessionStatus::Failed => AgentThreadStatus::Failed,
        TaskChildSessionStatus::Cancelled => AgentThreadStatus::Cancelled,
        TaskChildSessionStatus::Interrupted => AgentThreadStatus::Interrupted,
        TaskChildSessionStatus::Unavailable => AgentThreadStatus::Unavailable,
    }
}
