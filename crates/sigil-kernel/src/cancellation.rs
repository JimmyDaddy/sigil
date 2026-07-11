use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio::sync::Notify;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{DurableEventType, EventClass, JsonlSessionStore, Session, SessionStreamRecord};

/// Category of externally observable work guarded by cooperative run cancellation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RunEffectKind {
    ProviderRequest,
    Tool,
    ChildWork,
    Process,
    Socket,
    Redirect,
    Retry,
}

/// Whether an effect moves work forward or only cleans up already-admitted work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunEffectClass {
    Forward,
    Cleanup,
}

/// Outcome of waiting for all effects owned by one cancelled run to become quiet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunQuiescenceOutcome {
    Quiescent,
    TimedOut {
        active_effects: usize,
        active_tasks: usize,
    },
}

/// Durable cancellation target without embedding live runtime handles.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum RunCancellationTarget {
    Run,
    Task { task_id: String },
    AgentThread { thread_id: String },
}

/// Durable request that closes a root run's forward-effect gate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunCancellationRequestedEntry {
    pub request_id: String,
    pub run_scope_id: String,
    pub target: RunCancellationTarget,
    pub reason: String,
    pub requested_at_ms: u64,
    pub quiescence_deadline_ms: u64,
}

/// Honest terminal result of one durable cancellation request.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunCancellationTerminalOutcome {
    Cancelled,
    Interrupted,
}

/// Durable terminal record written only by the unique cancellation owner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunCancellationFinalizedEntry {
    pub request_id: String,
    pub run_scope_id: String,
    pub outcome: RunCancellationTerminalOutcome,
    pub cleanup_complete: bool,
    pub active_effects: usize,
    pub active_tasks: usize,
    pub reason: String,
    pub finalized_at_ms: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "record")]
enum DurableRunCancellationRecord {
    Requested(RunCancellationRequestedEntry),
    Finalized(RunCancellationFinalizedEntry),
}

/// Cloneable durable cancellation recorder backed by the session's linear writer.
#[derive(Debug, Clone)]
pub struct RunCancellationRecorder {
    store: JsonlSessionStore,
}

impl RunCancellationRecorder {
    pub(crate) fn new(store: JsonlSessionStore) -> Self {
        Self { store }
    }

    pub fn append_requested(&self, entry: &RunCancellationRequestedEntry) -> Result<bool> {
        let entry = entry.clone();
        self.store.append_event_if(
            DurableEventType::RunStatusChanged,
            EventClass::Critical,
            serde_json::to_value(DurableRunCancellationRecord::Requested(entry.clone()))?,
            move |records| {
                let existing = cancellation_records_from_stream(records)?;
                if let Some(request) = existing.iter().find_map(|record| match record {
                    DurableRunCancellationRecord::Requested(request)
                        if request.request_id == entry.request_id =>
                    {
                        Some(request)
                    }
                    _ => None,
                }) {
                    if request.run_scope_id != entry.run_scope_id {
                        bail!("cancellation request id is reused across run scopes");
                    }
                    return Ok(false);
                }
                Ok(true)
            },
        )
    }

    pub fn append_finalized(&self, entry: &RunCancellationFinalizedEntry) -> Result<bool> {
        let entry = entry.clone();
        self.store.append_event_if(
            DurableEventType::RunFinalized,
            EventClass::Critical,
            serde_json::to_value(DurableRunCancellationRecord::Finalized(entry.clone()))?,
            move |records| {
                let existing = cancellation_records_from_stream(records)?;
                let requested = existing.iter().any(|record| {
                    matches!(record, DurableRunCancellationRecord::Requested(request)
                        if request.request_id == entry.request_id
                            && request.run_scope_id == entry.run_scope_id)
                });
                if !requested {
                    bail!("cancellation terminal requires a matching durable request");
                }
                Ok(!existing.iter().any(|record| {
                    matches!(record, DurableRunCancellationRecord::Finalized(finalized)
                        if finalized.request_id == entry.request_id)
                }))
            },
        )
    }
}

#[derive(Default)]
struct RunCancellationState {
    scope_id: String,
    phase: AtomicU8,
    active_effects: AtomicUsize,
    active_tasks: AtomicUsize,
    cleanup_incomplete: AtomicBool,
    changed: Notify,
}

/// Cloneable child-facing cancellation capability for one run tree.
///
/// Effect admission and cancellation use the same atomic state. Once cancellation wins, new
/// effect guards cannot be created. Existing guards keep the run non-quiescent until they drop.
#[derive(Clone)]
pub struct RunCancellationHandle {
    state: Arc<RunCancellationState>,
}

impl fmt::Debug for RunCancellationHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RunCancellationHandle")
            .field("requested", &self.is_cancel_requested())
            .field("active_effects", &self.active_effects())
            .field("active_tasks", &self.active_tasks())
            .finish()
    }
}

impl RunCancellationHandle {
    fn reserve_cancel(&self) -> bool {
        self.state
            .phase
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    fn activate_reserved_cancel(&self) -> bool {
        let activated = self
            .state
            .phase
            .compare_exchange(1, 2, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok();
        if activated {
            self.state.changed.notify_waiters();
        }
        activated
    }

    #[must_use]
    pub fn scope_id(&self) -> &str {
        &self.state.scope_id
    }

    #[must_use]
    pub fn is_cancel_requested(&self) -> bool {
        matches!(self.state.phase.load(Ordering::SeqCst), 1 | 2)
    }

    #[must_use]
    pub fn can_request_cancel(&self) -> bool {
        self.state.phase.load(Ordering::SeqCst) == 0
    }

    /// Atomically gives natural run completion precedence over a later cancellation request.
    pub fn try_finalize_naturally(&self) -> bool {
        self.state
            .phase
            .compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    #[must_use]
    pub fn active_effects(&self) -> usize {
        self.state.active_effects.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn active_tasks(&self) -> usize {
        self.state.active_tasks.load(Ordering::SeqCst)
    }

    /// Resolves after cancellation is first requested.
    pub async fn cancelled(&self) {
        loop {
            let changed = self.state.changed.notified();
            if self.state.phase.load(Ordering::SeqCst) == 2 {
                return;
            }
            changed.await;
        }
    }

    pub fn mark_cleanup_incomplete(&self) {
        self.state.cleanup_incomplete.store(true, Ordering::SeqCst);
        self.state.changed.notify_waiters();
    }

    #[must_use]
    pub fn cleanup_complete(&self) -> bool {
        !self.state.cleanup_incomplete.load(Ordering::SeqCst)
    }

    /// Atomically admits one new effect unless cancellation has already been requested.
    pub fn begin_effect(
        &self,
        class: RunEffectClass,
        kind: RunEffectKind,
    ) -> Result<RunEffectGuard, RunCancellationRequested> {
        if class == RunEffectClass::Forward && self.is_cancel_requested() {
            return Err(RunCancellationRequested { kind });
        }
        self.state.active_effects.fetch_add(1, Ordering::SeqCst);
        if class == RunEffectClass::Forward && self.is_cancel_requested() {
            self.release_effect();
            return Err(RunCancellationRequested { kind });
        }
        Ok(RunEffectGuard {
            handle: self.clone(),
            class,
            kind,
        })
    }

    /// Registers one owned async task before it is spawned.
    pub fn register_task(&self) -> Result<RunTaskGuard, RunCancellationRequested> {
        if self.is_cancel_requested() {
            return Err(RunCancellationRequested {
                kind: RunEffectKind::ChildWork,
            });
        }
        self.state.active_tasks.fetch_add(1, Ordering::SeqCst);
        if self.is_cancel_requested() {
            self.release_task();
            return Err(RunCancellationRequested {
                kind: RunEffectKind::ChildWork,
            });
        }
        Ok(RunTaskGuard {
            handle: self.clone(),
        })
    }

    fn is_quiescent(&self) -> bool {
        self.active_effects() == 0 && self.active_tasks() == 0
    }

    fn wait_summary(&self) -> RunQuiescenceOutcome {
        RunQuiescenceOutcome::TimedOut {
            active_effects: self.active_effects(),
            active_tasks: self.active_tasks(),
        }
    }

    async fn wait_for_quiescence(&self, timeout: Duration) -> RunQuiescenceOutcome {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let changed = self.state.changed.notified();
            if self.is_quiescent() {
                return RunQuiescenceOutcome::Quiescent;
            }
            if tokio::time::timeout_at(deadline, changed).await.is_err() {
                return self.wait_summary();
            }
        }
    }

    fn release_effect(&self) {
        let previous = self.state.active_effects.fetch_sub(1, Ordering::SeqCst);
        debug_assert!(previous > 0, "run cancellation effect count underflow");
        if previous == 1 {
            self.state.changed.notify_waiters();
        }
    }

    fn release_task(&self) {
        let previous = self.state.active_tasks.fetch_sub(1, Ordering::SeqCst);
        debug_assert!(previous > 0, "run cancellation task count underflow");
        if previous == 1 {
            self.state.changed.notify_waiters();
        }
    }
}

/// Unique root authority that requests cancellation and confirms terminal quiescence.
///
/// This type is intentionally not `Clone`; child work receives only [`RunCancellationHandle`].
#[derive(Debug)]
pub struct RunCancellationOwner {
    handle: RunCancellationHandle,
}

impl Default for RunCancellationOwner {
    fn default() -> Self {
        Self::new()
    }
}

impl RunCancellationOwner {
    #[must_use]
    pub fn new() -> Self {
        let state = RunCancellationState {
            scope_id: uuid::Uuid::new_v4().to_string(),
            ..RunCancellationState::default()
        };
        Self {
            handle: RunCancellationHandle {
                state: Arc::new(state),
            },
        }
    }

    #[must_use]
    pub fn handle(&self) -> RunCancellationHandle {
        self.handle.clone()
    }

    pub fn request_cancel(&self) -> bool {
        self.reserve_cancel() && self.activate_reserved_cancel()
    }

    /// Creates a root-owned one-shot hook for deterministic hard-budget exhaustion.
    ///
    /// Child work receives only the budget handle; the cancellation authority remains captured by
    /// the root-created hook and cannot be recovered from that handle.
    #[must_use]
    pub fn budget_cancellation_hook(&self) -> Arc<dyn Fn() + Send + Sync> {
        let handle = self.handle.clone();
        Arc::new(move || {
            if handle.reserve_cancel() {
                handle.activate_reserved_cancel();
            }
        })
    }

    pub fn reserve_cancel(&self) -> bool {
        self.handle.reserve_cancel()
    }

    pub fn activate_reserved_cancel(&self) -> bool {
        self.handle.activate_reserved_cancel()
    }

    pub async fn wait_for_quiescence(&self, timeout: Duration) -> RunQuiescenceOutcome {
        self.handle.wait_for_quiescence(timeout).await
    }

    #[must_use]
    pub fn is_quiescent(&self) -> bool {
        self.handle.is_quiescent()
    }

    #[must_use]
    pub fn cleanup_complete(&self) -> bool {
        self.handle.cleanup_complete()
    }
}

/// One admitted effect. Dropping it contributes to the run's quiescence proof.
#[derive(Debug)]
pub struct RunEffectGuard {
    handle: RunCancellationHandle,
    class: RunEffectClass,
    kind: RunEffectKind,
}

impl RunEffectGuard {
    #[must_use]
    pub fn kind(&self) -> RunEffectKind {
        self.kind
    }

    #[must_use]
    pub fn class(&self) -> RunEffectClass {
        self.class
    }
}

impl Drop for RunEffectGuard {
    fn drop(&mut self) {
        self.handle.release_effect();
    }
}

/// One owned task registered with the root cancellation scope.
#[derive(Debug)]
pub struct RunTaskGuard {
    handle: RunCancellationHandle,
}

impl Drop for RunTaskGuard {
    fn drop(&mut self) {
        self.handle.release_task();
    }
}

/// Appends an idempotent durable cancellation request before cleanup begins.
pub fn append_run_cancellation_requested(
    session: &mut Session,
    entry: &RunCancellationRequestedEntry,
) -> Result<bool> {
    session.run_cancellation_recorder()?.append_requested(entry)
}

/// Appends the exact-one terminal outcome for a durable cancellation request.
pub fn append_run_cancellation_finalized(
    session: &mut Session,
    entry: &RunCancellationFinalizedEntry,
) -> Result<bool> {
    session.run_cancellation_recorder()?.append_finalized(entry)
}

/// Closes cancellation requests left open by a crash as interrupted and cleanup-unconfirmed.
pub fn reconcile_unfinished_run_cancellations(
    session: &mut Session,
    finalized_at_ms: u64,
) -> Result<Vec<RunCancellationFinalizedEntry>> {
    let records = cancellation_records(session)?;
    let finalized = records
        .iter()
        .filter_map(|record| match record {
            DurableRunCancellationRecord::Finalized(entry) => Some(entry.request_id.as_str()),
            DurableRunCancellationRecord::Requested(_) => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    let pending = records
        .iter()
        .filter_map(|record| match record {
            DurableRunCancellationRecord::Requested(entry)
                if !finalized.contains(entry.request_id.as_str())
                    && finalized_at_ms >= entry.quiescence_deadline_ms =>
            {
                Some(entry.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut recovered = Vec::new();
    for request in pending {
        let entry = RunCancellationFinalizedEntry {
            request_id: request.request_id,
            run_scope_id: request.run_scope_id,
            outcome: RunCancellationTerminalOutcome::Interrupted,
            cleanup_complete: false,
            active_effects: 0,
            active_tasks: 0,
            reason: "cancellation recovery could not confirm cleanup".to_owned(),
            finalized_at_ms,
        };
        if append_run_cancellation_finalized(session, &entry)? {
            recovered.push(entry);
        }
    }
    Ok(recovered)
}

fn cancellation_records(session: &Session) -> Result<Vec<DurableRunCancellationRecord>> {
    let path = session
        .store_path()
        .context("run cancellation requires a durable session store")?;
    cancellation_records_from_path(path)
}

fn cancellation_records_from_path(
    path: &std::path::Path,
) -> Result<Vec<DurableRunCancellationRecord>> {
    cancellation_records_from_stream(&JsonlSessionStore::read_event_records(path)?)
}

fn cancellation_records_from_stream(
    stream: &[SessionStreamRecord],
) -> Result<Vec<DurableRunCancellationRecord>> {
    let mut records = Vec::new();
    for record in stream {
        let SessionStreamRecord::Stored(event) = record else {
            continue;
        };
        if !matches!(
            DurableEventType::from_event_type(&event.event_type),
            Some(DurableEventType::RunStatusChanged | DurableEventType::RunFinalized)
        ) {
            continue;
        }
        if let Ok(record) = serde_json::from_value(event.payload.clone()) {
            records.push(record);
        }
    }
    let mut request_scopes = std::collections::BTreeMap::new();
    for record in &records {
        if let DurableRunCancellationRecord::Requested(entry) = record
            && let Some(existing) =
                request_scopes.insert(entry.request_id.as_str(), entry.run_scope_id.as_str())
            && existing != entry.run_scope_id
        {
            bail!("cancellation request id is reused across run scopes");
        }
    }
    Ok(records)
}

/// Stable error returned when code attempts to start a new effect after cancellation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("run cancellation requested; refusing new {kind:?} effect")]
pub struct RunCancellationRequested {
    pub kind: RunEffectKind,
}

#[cfg(test)]
#[path = "tests/cancellation_tests.rs"]
mod tests;
