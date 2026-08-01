use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
    marker::PhantomData,
    sync::{Arc, Mutex, mpsc},
};

use sigil_kernel::{
    AgentThreadId, TerminalLifecycleUpdateV2, TerminalTaskId,
    session::{ActiveProjectionFamily, ActiveProjectionNotice, ActiveProjectionObserver},
};
use sigil_runtime::ProviderStatusTaskResult;

use super::{
    mcp_event_bridge::McpRuntimeEvent,
    protocol::{WorkerCommand, is_urgent_worker_command},
    worker_loop::{
        ArtifactGcTaskResult, CompactionPreparationTaskResult, McpOAuthTaskResult, RunTaskResult,
    },
};

pub(in crate::runner) const MAX_PENDING_MCP_RUNTIME_EVENTS: usize = 128;
const MAX_OBSERVED_MCP_LIST_CHANGE_SERVERS: usize = sigil_runtime::MAX_MCP_SERVER_DECLARATIONS;

pub(in crate::runner) enum WorkerEvent {
    Command(WorkerCommand),
    RunCompleted(Box<RunTaskResult>),
    CompactionPrepared(CompactionPreparationTaskResult),
    ProviderStatusResolved(ProviderStatusTaskResult),
    McpOAuthCompleted(McpOAuthTaskResult),
    ArtifactGcCompleted(ArtifactGcTaskResult),
    McpRuntimeReady(WorkerMcpRuntimeEventSender),
    TerminalLifecycleReady(WorkerTerminalLifecycleEventSender),
    Wake(WorkerWakeCoalescer),
    TimerDue,
    ControlWake,
}

#[derive(Debug, Clone)]
pub(in crate::runner) struct RoutedTerminalLifecycleUpdate {
    pub(in crate::runner) session_scope_id: String,
    pub(in crate::runner) run_id: String,
    pub(in crate::runner) update: TerminalLifecycleUpdateV2,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct TerminalLifecycleEventKey {
    session_scope_id: String,
    task_id: TerminalTaskId,
}

#[derive(Clone)]
pub(in crate::runner) struct WorkerTerminalLifecycleEventSender {
    inner: Arc<WorkerTerminalLifecycleEventSenderInner>,
}

struct WorkerTerminalLifecycleEventSenderInner {
    event_tx: mpsc::Sender<WorkerEvent>,
    slot: Mutex<WorkerTerminalLifecycleEventSlot>,
}

struct WorkerTerminalLifecycleEventSlot {
    wake_queued: bool,
    pending: BTreeMap<TerminalLifecycleEventKey, RoutedTerminalLifecycleUpdate>,
    order: VecDeque<TerminalLifecycleEventKey>,
}

impl WorkerTerminalLifecycleEventSender {
    pub(in crate::runner) fn new(event_tx: mpsc::Sender<WorkerEvent>) -> Self {
        Self {
            inner: Arc::new(WorkerTerminalLifecycleEventSenderInner {
                event_tx,
                slot: Mutex::new(WorkerTerminalLifecycleEventSlot {
                    wake_queued: false,
                    pending: BTreeMap::new(),
                    order: VecDeque::new(),
                }),
            }),
        }
    }

    pub(in crate::runner) fn send(
        &self,
        routed: RoutedTerminalLifecycleUpdate,
    ) -> Result<(), WorkerEventSendError> {
        let key = TerminalLifecycleEventKey {
            session_scope_id: routed.session_scope_id.clone(),
            task_id: routed.update.event.task_id.clone(),
        };
        let should_publish = {
            let mut slot = self.lock_slot();
            if slot.pending.get(&key).is_some_and(|pending| {
                pending.update.event.generation >= routed.update.event.generation
            }) {
                return Ok(());
            }
            if slot.pending.insert(key.clone(), routed).is_none() {
                slot.order.push_back(key);
            }
            if slot.wake_queued {
                false
            } else {
                slot.wake_queued = true;
                true
            }
        };
        if should_publish
            && self
                .inner
                .event_tx
                .send(WorkerEvent::TerminalLifecycleReady(self.clone()))
                .is_err()
        {
            self.lock_slot().wake_queued = false;
            return Err(WorkerEventSendError);
        }
        Ok(())
    }

    fn drain(&self) -> VecDeque<RoutedTerminalLifecycleUpdate> {
        let mut slot = self.lock_slot();
        let mut drained = VecDeque::with_capacity(slot.pending.len());
        while let Some(key) = slot.order.pop_front() {
            if let Some(update) = slot.pending.remove(&key) {
                drained.push_back(update);
            }
        }
        slot.wake_queued = false;
        drained
    }

    fn lock_slot(&self) -> std::sync::MutexGuard<'_, WorkerTerminalLifecycleEventSlot> {
        self.inner
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl fmt::Debug for WorkerTerminalLifecycleEventSender {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkerTerminalLifecycleEventSender")
            .field("pending", &self.lock_slot().pending.len())
            .finish_non_exhaustive()
    }
}

pub(in crate::runner) type WorkerEventInbox = (
    mpsc::Sender<WorkerEvent>,
    mpsc::Receiver<WorkerEvent>,
    mpsc::Receiver<WorkerCommand>,
);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum McpRuntimeEventKey {
    Progress {
        server_name: String,
        progress_token: String,
    },
    ListChanged {
        server_name: String,
    },
}

impl McpRuntimeEventKey {
    fn is_progress(&self) -> bool {
        matches!(self, Self::Progress { .. })
    }
}

#[derive(Clone)]
pub(in crate::runner) struct WorkerMcpRuntimeEventSender {
    inner: Arc<WorkerMcpRuntimeEventSenderInner>,
}

struct WorkerMcpRuntimeEventSenderInner {
    event_tx: mpsc::Sender<WorkerEvent>,
    slot: Mutex<WorkerMcpRuntimeEventSlot>,
}

struct WorkerMcpRuntimeEventSlot {
    wake_queued: bool,
    resync_observed_servers: bool,
    observed_list_change_servers: BTreeSet<String>,
    pending: BTreeMap<McpRuntimeEventKey, McpRuntimeEvent>,
    order: VecDeque<McpRuntimeEventKey>,
}

impl WorkerMcpRuntimeEventSender {
    pub(in crate::runner) fn new(event_tx: mpsc::Sender<WorkerEvent>) -> Self {
        Self {
            inner: Arc::new(WorkerMcpRuntimeEventSenderInner {
                event_tx,
                slot: Mutex::new(WorkerMcpRuntimeEventSlot {
                    wake_queued: false,
                    resync_observed_servers: false,
                    observed_list_change_servers: BTreeSet::new(),
                    pending: BTreeMap::new(),
                    order: VecDeque::new(),
                }),
            }),
        }
    }

    pub(in crate::runner) fn send(
        &self,
        event: McpRuntimeEvent,
    ) -> Result<(), WorkerEventSendError> {
        let key = match &event {
            McpRuntimeEvent::Progress(notification) => McpRuntimeEventKey::Progress {
                server_name: notification.server_name.clone(),
                progress_token: notification.progress_token.clone(),
            },
            McpRuntimeEvent::ListChanged(notification) => McpRuntimeEventKey::ListChanged {
                server_name: notification.server_name.clone(),
            },
        };
        let should_publish = {
            let mut slot = self.lock_slot();
            if let McpRuntimeEvent::ListChanged(notification) = &event {
                // This is lifecycle knowledge, not pending-event backlog. Overflow recovery must
                // only refresh connections that have actually emitted after activation, so lazy
                // MCP declarations remain deferred.
                if slot
                    .observed_list_change_servers
                    .contains(&notification.server_name)
                    || slot.observed_list_change_servers.len()
                        < MAX_OBSERVED_MCP_LIST_CHANGE_SERVERS
                {
                    slot.observed_list_change_servers
                        .insert(notification.server_name.clone());
                }
            }
            if slot.pending.contains_key(&key) {
                slot.order.retain(|pending_key| pending_key != &key);
            } else if slot.pending.len() >= MAX_PENDING_MCP_RUNTIME_EVENTS {
                // Prefer dropping lossy progress. If the hard cap contains only dirty-server
                // signals, collapse any evicted signal into one conservative resync-all marker so
                // no MCP server can remain permanently stale.
                let progress_index = slot.order.iter().position(McpRuntimeEventKey::is_progress);
                let evicted = progress_index
                    .and_then(|index| slot.order.remove(index))
                    .or_else(|| slot.order.pop_front());
                if let Some(evicted) = evicted {
                    if matches!(evicted, McpRuntimeEventKey::ListChanged { .. }) {
                        slot.resync_observed_servers = true;
                    }
                    slot.pending.remove(&evicted);
                }
            }
            slot.pending.insert(key.clone(), event);
            slot.order.push_back(key);
            if slot.wake_queued {
                false
            } else {
                slot.wake_queued = true;
                true
            }
        };
        if should_publish
            && self
                .inner
                .event_tx
                .send(WorkerEvent::McpRuntimeReady(self.clone()))
                .is_err()
        {
            self.lock_slot().wake_queued = false;
            return Err(WorkerEventSendError);
        }
        Ok(())
    }

    fn drain(&self) -> (VecDeque<McpRuntimeEvent>, BTreeSet<String>) {
        let mut slot = self.lock_slot();
        let mut drained = VecDeque::with_capacity(slot.pending.len());
        while let Some(key) = slot.order.pop_front() {
            if let Some(event) = slot.pending.remove(&key) {
                drained.push_back(event);
            }
        }
        slot.wake_queued = false;
        let resync_servers = if std::mem::take(&mut slot.resync_observed_servers) {
            slot.observed_list_change_servers.clone()
        } else {
            BTreeSet::new()
        };
        (drained, resync_servers)
    }

    #[cfg(test)]
    pub(in crate::runner) fn pending_len(&self) -> usize {
        self.lock_slot().pending.len()
    }

    fn lock_slot(&self) -> std::sync::MutexGuard<'_, WorkerMcpRuntimeEventSlot> {
        self.inner
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl fmt::Debug for WorkerMcpRuntimeEventSender {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkerMcpRuntimeEventSender")
            .field("pending", &self.lock_slot().pending.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub(in crate::runner) struct WorkerWakeCoalescer {
    inner: Arc<WorkerWakeCoalescerInner>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runner) struct WorkerProjectionBinding {
    pub(in crate::runner) session_scope_id: String,
    pub(in crate::runner) observer_id: u64,
}

struct WorkerWakeCoalescerInner {
    event_tx: mpsc::Sender<WorkerEvent>,
    slot: Mutex<WorkerWakeSlot>,
}

struct WorkerWakeSlot {
    session_scope_id: Option<String>,
    observer_id: u64,
    wake_queued: bool,
    session_projection_invalid: bool,
    session_projection_families: BTreeSet<ActiveProjectionFamily>,
    provider_route_diagnostics_changed: bool,
    task_completion_progress_changed: bool,
    background_agents: BTreeSet<AgentThreadId>,
}

struct WorkerWakeBatch {
    session_scope_id: Option<String>,
    observer_id: u64,
    session_projection_invalid: bool,
    session_projection_families: BTreeSet<ActiveProjectionFamily>,
    provider_route_diagnostics_changed: bool,
    task_completion_progress_changed: bool,
    background_agents: BTreeSet<AgentThreadId>,
}

impl WorkerWakeBatch {
    fn has_changes(&self) -> bool {
        self.session_projection_invalid
            || !self.session_projection_families.is_empty()
            || self.provider_route_diagnostics_changed
            || self.task_completion_progress_changed
            || !self.background_agents.is_empty()
    }
}

impl WorkerWakeCoalescer {
    pub(in crate::runner) fn new(
        event_tx: mpsc::Sender<WorkerEvent>,
        session_scope_id: Option<String>,
    ) -> Self {
        Self {
            inner: Arc::new(WorkerWakeCoalescerInner {
                event_tx,
                slot: Mutex::new(WorkerWakeSlot {
                    session_scope_id,
                    observer_id: 1,
                    wake_queued: false,
                    session_projection_invalid: false,
                    session_projection_families: BTreeSet::new(),
                    provider_route_diagnostics_changed: false,
                    task_completion_progress_changed: false,
                    background_agents: BTreeSet::new(),
                }),
            }),
        }
    }

    pub(in crate::runner) fn current_projection_binding(&self) -> Option<WorkerProjectionBinding> {
        let slot = self.lock_slot();
        slot.session_scope_id
            .as_ref()
            .map(|session_scope_id| WorkerProjectionBinding {
                session_scope_id: session_scope_id.clone(),
                observer_id: slot.observer_id,
            })
    }

    /// Coalesces repeated projection publications for the active observer into one inbox token.
    pub(in crate::runner) fn notify_session_projection(
        &self,
        binding: &WorkerProjectionBinding,
        invalid: bool,
        changed_families: &BTreeSet<ActiveProjectionFamily>,
    ) {
        self.notify(|slot| {
            if slot.session_scope_id.as_deref() == Some(binding.session_scope_id.as_str())
                && slot.observer_id == binding.observer_id
            {
                slot.session_projection_invalid |= invalid;
                let previous_len = slot.session_projection_families.len();
                slot.session_projection_families
                    .extend(changed_families.iter().copied());
                invalid || slot.session_projection_families.len() != previous_len
            } else {
                false
            }
        });
    }

    pub(in crate::runner) fn notify_supervisor(
        &self,
        change: sigil_runtime::AgentSupervisorChange,
    ) {
        self.notify(|slot| {
            match change {
                sigil_runtime::AgentSupervisorChange::ProviderRouteDiagnostics => {
                    slot.provider_route_diagnostics_changed = true;
                }
                sigil_runtime::AgentSupervisorChange::TaskCompletionProgress => {
                    slot.task_completion_progress_changed = true;
                }
            }
            true
        });
    }

    pub(in crate::runner) fn notify_background_agent(&self, thread_id: &AgentThreadId) {
        self.notify(|slot| slot.background_agents.insert(thread_id.clone()));
    }

    pub(in crate::runner) fn switch_session_scope(
        &self,
        session_scope_id: String,
    ) -> WorkerProjectionBinding {
        let mut slot = self.lock_slot();
        slot.session_scope_id = Some(session_scope_id);
        slot.observer_id = slot.observer_id.saturating_add(1);
        slot.session_projection_invalid = false;
        slot.session_projection_families.clear();
        slot.provider_route_diagnostics_changed = false;
        slot.task_completion_progress_changed = false;
        slot.background_agents.clear();
        // Keep wake_queued: an already queued token will observe the new slot. If there is no
        // token, the next producer sets this flag and publishes one.
        WorkerProjectionBinding {
            session_scope_id: slot
                .session_scope_id
                .clone()
                .expect("session scope was installed"),
            observer_id: slot.observer_id,
        }
    }

    fn notify(&self, update: impl FnOnce(&mut WorkerWakeSlot) -> bool) {
        let should_publish = {
            let mut slot = self.lock_slot();
            let changed = update(&mut slot);
            if !changed || slot.wake_queued {
                false
            } else {
                slot.wake_queued = true;
                true
            }
        };
        if should_publish
            && self
                .inner
                .event_tx
                .send(WorkerEvent::Wake(self.clone()))
                .is_err()
        {
            self.lock_slot().wake_queued = false;
        }
    }

    fn drain(&self) -> WorkerWakeBatch {
        let mut slot = self.lock_slot();
        let batch = WorkerWakeBatch {
            session_scope_id: slot.session_scope_id.clone(),
            observer_id: slot.observer_id,
            session_projection_invalid: std::mem::take(&mut slot.session_projection_invalid),
            session_projection_families: std::mem::take(&mut slot.session_projection_families),
            provider_route_diagnostics_changed: std::mem::take(
                &mut slot.provider_route_diagnostics_changed,
            ),
            task_completion_progress_changed: std::mem::take(
                &mut slot.task_completion_progress_changed,
            ),
            background_agents: std::mem::take(&mut slot.background_agents),
        };
        slot.wake_queued = false;
        batch
    }

    fn is_current(&self, batch: &WorkerWakeBatch) -> bool {
        let slot = self.lock_slot();
        slot.session_scope_id == batch.session_scope_id && slot.observer_id == batch.observer_id
    }

    fn lock_slot(&self) -> std::sync::MutexGuard<'_, WorkerWakeSlot> {
        self.inner
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub(in crate::runner) struct WorkerActiveProjectionObserver {
    wake_coalescer: WorkerWakeCoalescer,
    binding: WorkerProjectionBinding,
}

impl WorkerActiveProjectionObserver {
    pub(in crate::runner) fn new(
        wake_coalescer: WorkerWakeCoalescer,
        binding: WorkerProjectionBinding,
    ) -> Self {
        Self {
            wake_coalescer,
            binding,
        }
    }
}

impl ActiveProjectionObserver for WorkerActiveProjectionObserver {
    fn active_projection_changed(&self, notice: ActiveProjectionNotice) {
        if notice.frontier.session_id() != self.binding.session_scope_id {
            return;
        }
        let relevant_families = notice
            .changed_families
            .iter()
            .copied()
            .filter(|family| {
                matches!(
                    family,
                    ActiveProjectionFamily::Queue
                        | ActiveProjectionFamily::Task
                        | ActiveProjectionFamily::AgentContinuation
                        | ActiveProjectionFamily::TerminalTask
                        | ActiveProjectionFamily::Usage
                        | ActiveProjectionFamily::Readiness
                        | ActiveProjectionFamily::Compaction
                        | ActiveProjectionFamily::ToolOutputPressure
                )
            })
            .collect::<BTreeSet<_>>();
        if notice.valid && relevant_families.is_empty() {
            return;
        }
        self.wake_coalescer.notify_session_projection(
            &self.binding,
            !notice.valid,
            &relevant_families,
        );
    }
}

pub(in crate::runner) struct WorkerWakeReadiness {
    pub(in crate::runner) any: bool,
    pub(in crate::runner) projection_invalid: bool,
    pub(in crate::runner) projection_families: BTreeSet<ActiveProjectionFamily>,
}

impl WorkerWakeReadiness {
    pub(in crate::runner) fn task_guidance_dirty(&self) -> bool {
        self.projection_invalid
            || self
                .projection_families
                .contains(&ActiveProjectionFamily::Queue)
            || self
                .projection_families
                .contains(&ActiveProjectionFamily::Task)
    }

    pub(in crate::runner) fn conversation_queue_dirty(&self) -> bool {
        self.projection_invalid
            || self
                .projection_families
                .contains(&ActiveProjectionFamily::Queue)
    }

    pub(in crate::runner) fn tool_output_pressure_dirty(&self) -> bool {
        self.projection_invalid
            || self
                .projection_families
                .contains(&ActiveProjectionFamily::ToolOutputPressure)
    }
}

pub(in crate::runner) struct WorkerReadiness {
    urgent_commands: VecDeque<WorkerCommand>,
    ordinary_commands: VecDeque<WorkerCommand>,
    pub(in crate::runner) run_results: VecDeque<RunTaskResult>,
    pub(in crate::runner) compaction_results: VecDeque<CompactionPreparationTaskResult>,
    pub(in crate::runner) provider_status_results: VecDeque<ProviderStatusTaskResult>,
    pub(in crate::runner) mcp_oauth_results: VecDeque<McpOAuthTaskResult>,
    pub(in crate::runner) artifact_gc_results: VecDeque<ArtifactGcTaskResult>,
    pub(in crate::runner) mcp_runtime_events: VecDeque<McpRuntimeEvent>,
    pub(in crate::runner) mcp_resync_servers: BTreeSet<String>,
    pub(in crate::runner) terminal_lifecycle_updates: VecDeque<RoutedTerminalLifecycleUpdate>,
    wakes: VecDeque<WorkerWakeBatch>,
    timer_due: bool,
}

impl WorkerReadiness {
    pub(in crate::runner) fn new() -> Self {
        Self {
            urgent_commands: VecDeque::new(),
            ordinary_commands: VecDeque::new(),
            run_results: VecDeque::new(),
            compaction_results: VecDeque::new(),
            provider_status_results: VecDeque::new(),
            mcp_oauth_results: VecDeque::new(),
            artifact_gc_results: VecDeque::new(),
            mcp_runtime_events: VecDeque::new(),
            mcp_resync_servers: BTreeSet::new(),
            terminal_lifecycle_updates: VecDeque::new(),
            wakes: VecDeque::new(),
            timer_due: false,
        }
    }

    pub(in crate::runner) fn ingest(&mut self, event: WorkerEvent) {
        match event {
            WorkerEvent::Command(command) => self.push_command(command),
            WorkerEvent::RunCompleted(result) => self.run_results.push_back(*result),
            WorkerEvent::CompactionPrepared(result) => self.compaction_results.push_back(result),
            WorkerEvent::ProviderStatusResolved(result) => {
                self.provider_status_results.push_back(result);
            }
            WorkerEvent::McpOAuthCompleted(result) => self.mcp_oauth_results.push_back(result),
            WorkerEvent::ArtifactGcCompleted(result) => {
                self.artifact_gc_results.push_back(result);
            }
            WorkerEvent::McpRuntimeReady(sender) => {
                let (events, resync_servers) = sender.drain();
                self.mcp_runtime_events.extend(events);
                self.mcp_resync_servers.extend(resync_servers);
            }
            WorkerEvent::TerminalLifecycleReady(sender) => {
                self.terminal_lifecycle_updates.extend(sender.drain());
            }
            WorkerEvent::Wake(wake) => {
                let batch = wake.drain();
                if batch.has_changes() {
                    self.wakes.push_back(batch);
                }
            }
            WorkerEvent::TimerDue => self.timer_due = true,
            WorkerEvent::ControlWake => {}
        }
    }

    pub(in crate::runner) fn pop_urgent_command(&mut self) -> Option<WorkerCommand> {
        self.urgent_commands.pop_front()
    }

    pub(in crate::runner) fn pop_ordinary_command(&mut self) -> Option<WorkerCommand> {
        self.ordinary_commands.pop_front()
    }

    pub(in crate::runner) fn pop_ordinary_command_unless(
        &mut self,
        blocked: impl FnOnce(&WorkerCommand) -> bool,
    ) -> Option<WorkerCommand> {
        if self.ordinary_commands.front().is_some_and(blocked) {
            None
        } else {
            self.ordinary_commands.pop_front()
        }
    }

    pub(in crate::runner) fn pop_projection_recovery_command(&mut self) -> Option<WorkerCommand> {
        let index = self.ordinary_commands.iter().position(|command| {
            matches!(
                command,
                WorkerCommand::StartNewSession { .. } | WorkerCommand::SwitchSession { .. }
            )
        })?;
        self.ordinary_commands.remove(index)
    }

    pub(in crate::runner) fn has_ready_work(&self) -> bool {
        !self.run_results.is_empty()
            || !self.compaction_results.is_empty()
            || !self.provider_status_results.is_empty()
            || !self.mcp_oauth_results.is_empty()
            || !self.artifact_gc_results.is_empty()
            || !self.mcp_runtime_events.is_empty()
            || !self.mcp_resync_servers.is_empty()
            || !self.terminal_lifecycle_updates.is_empty()
            || !self.wakes.is_empty()
            || self.timer_due
    }

    /// Returns whether readiness contains lifecycle or authority work that outranks an ordinary
    /// command. Observational MCP refresh, usage/readiness projection changes, diagnostics and
    /// timers deliberately remain below the ordinary command lane.
    pub(in crate::runner) fn has_priority_ready_work(&self) -> bool {
        !self.run_results.is_empty()
            || !self.compaction_results.is_empty()
            || !self.provider_status_results.is_empty()
            || !self.mcp_oauth_results.is_empty()
            || !self.terminal_lifecycle_updates.is_empty()
            || self.wakes.iter().any(|batch| {
                batch.session_projection_invalid
                    || !batch.background_agents.is_empty()
                    || batch.session_projection_families.iter().any(|family| {
                        matches!(
                            family,
                            ActiveProjectionFamily::Queue
                                | ActiveProjectionFamily::Task
                                | ActiveProjectionFamily::AgentContinuation
                                | ActiveProjectionFamily::TerminalTask
                        )
                    })
            })
    }

    pub(in crate::runner) fn take_timer_due(&mut self) -> bool {
        std::mem::take(&mut self.timer_due)
    }

    pub(in crate::runner) fn take_wake_readiness(
        &mut self,
        coalescer: &WorkerWakeCoalescer,
    ) -> WorkerWakeReadiness {
        let mut readiness = WorkerWakeReadiness {
            any: false,
            projection_invalid: false,
            projection_families: BTreeSet::new(),
        };
        for batch in self
            .wakes
            .iter()
            .filter(|batch| coalescer.is_current(batch))
        {
            readiness.any = true;
            readiness.projection_invalid |= batch.session_projection_invalid;
            readiness
                .projection_families
                .extend(batch.session_projection_families.iter().copied());
        }
        self.wakes.clear();
        readiness
    }

    fn push_command(&mut self, command: WorkerCommand) {
        if is_urgent_worker_command(&command) {
            self.urgent_commands.push_back(command);
        } else {
            self.ordinary_commands.push_back(command);
        }
    }
}

pub(in crate::runner) struct WorkerEventPayloadSender<T> {
    event_tx: mpsc::Sender<WorkerEvent>,
    wrap: fn(T) -> WorkerEvent,
    marker: PhantomData<fn(T)>,
}

impl<T> Clone for WorkerEventPayloadSender<T> {
    fn clone(&self) -> Self {
        Self {
            event_tx: self.event_tx.clone(),
            wrap: self.wrap,
            marker: PhantomData,
        }
    }
}

impl<T> fmt::Debug for WorkerEventPayloadSender<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkerEventPayloadSender")
            .finish_non_exhaustive()
    }
}

impl<T> WorkerEventPayloadSender<T> {
    fn new(event_tx: mpsc::Sender<WorkerEvent>, wrap: fn(T) -> WorkerEvent) -> Self {
        Self {
            event_tx,
            wrap,
            marker: PhantomData,
        }
    }

    pub(in crate::runner) fn send(&self, payload: T) -> Result<(), WorkerEventSendError> {
        self.event_tx
            .send((self.wrap)(payload))
            .map_err(|_| WorkerEventSendError)
    }
}

impl WorkerEventPayloadSender<RunTaskResult> {
    pub(in crate::runner) fn run(event_tx: mpsc::Sender<WorkerEvent>) -> Self {
        Self::new(event_tx, |result| {
            WorkerEvent::RunCompleted(Box::new(result))
        })
    }
}

impl WorkerEventPayloadSender<CompactionPreparationTaskResult> {
    pub(in crate::runner) fn compaction(event_tx: mpsc::Sender<WorkerEvent>) -> Self {
        Self::new(event_tx, WorkerEvent::CompactionPrepared)
    }
}

impl WorkerEventPayloadSender<McpOAuthTaskResult> {
    pub(in crate::runner) fn mcp_oauth(event_tx: mpsc::Sender<WorkerEvent>) -> Self {
        Self::new(event_tx, WorkerEvent::McpOAuthCompleted)
    }
}

impl WorkerEventPayloadSender<ArtifactGcTaskResult> {
    pub(in crate::runner) fn artifact_gc(event_tx: mpsc::Sender<WorkerEvent>) -> Self {
        Self::new(event_tx, WorkerEvent::ArtifactGcCompleted)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runner) struct WorkerEventSendError;

impl fmt::Display for WorkerEventSendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("worker event receiver is disconnected")
    }
}

impl std::error::Error for WorkerEventSendError {}

#[cfg(test)]
#[path = "tests/worker_event_tests.rs"]
mod tests;
