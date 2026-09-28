use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tokio::{runtime::Runtime, task::JoinHandle};

use super::{
    IdleAutoCompactionPreparation, IdleAutoCompactionState, PendingLocalV2Compaction,
    PendingV2Compaction, QueuedConversationPreTurnAdmission,
};
use crate::runner::V2CompactionReview;
use crate::runner::worker_event::WorkerEventPayloadSender;
use sigil_kernel::{ConversationInputQueueId, Session, session::ActiveProjectionFrontier};

pub(in crate::runner) struct ManualV2CompactionPreparation {
    pub(in crate::runner) review: V2CompactionReview,
    pub(in crate::runner) local_preview: Option<PendingLocalV2Compaction>,
    pub(in crate::runner) pending: Option<PendingV2Compaction>,
    pub(in crate::runner) apply_source: super::V2CompactionApplySource,
}

pub(in crate::runner) struct IdleV2CompactionPreparation {
    pub(in crate::runner) state: IdleAutoCompactionState,
    pub(in crate::runner) preparation: Result<IdleAutoCompactionPreparation, String>,
    pub(in crate::runner) session: sigil_kernel::Session,
}

pub(in crate::runner) struct PreTurnV2CompactionPreparation {
    pub(in crate::runner) queue_id: ConversationInputQueueId,
    pub(in crate::runner) admission: QueuedConversationPreTurnAdmission,
    pub(in crate::runner) session: Option<Session>,
    pub(in crate::runner) prepared_frontier: ActiveProjectionFrontier,
}

pub(in crate::runner) struct OverflowV2CompactionPreparation {
    pub(in crate::runner) source_physical_attempt_id: String,
    pub(in crate::runner) source_logical_run_id: String,
    pub(in crate::runner) original_run_error: String,
    pub(in crate::runner) preparation: Result<PendingV2Compaction, String>,
}

pub(in crate::runner) enum CompactionPreparationTaskResult {
    Manual {
        request_id: u64,
        session_scope_id: String,
        result: Result<Box<ManualV2CompactionPreparation>, String>,
    },
    Idle {
        request_id: u64,
        session_scope_id: String,
        result: Result<Box<IdleV2CompactionPreparation>, String>,
    },
    PreTurn {
        request_id: u64,
        session_scope_id: String,
        result: Result<Box<PreTurnV2CompactionPreparation>, String>,
    },
    Overflow {
        request_id: u64,
        session_scope_id: String,
        result: Result<Box<OverflowV2CompactionPreparation>, String>,
    },
}

#[derive(Default)]
pub(in crate::runner) struct CompactionPreparationTaskManager {
    active: Option<ActiveCompactionPreparationTask>,
    retired: Vec<JoinHandle<()>>,
    task_panicked: bool,
}

struct ActiveCompactionPreparationTask {
    request_id: u64,
    session_scope_id: String,
    cancelled: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

impl CompactionPreparationTaskManager {
    pub(in crate::runner) fn new() -> Self {
        Self::default()
    }

    pub(in crate::runner) fn start_manual<F>(
        &mut self,
        runtime: &Runtime,
        request_id: u64,
        session_scope_id: String,
        session_attachment: Arc<
            sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease,
        >,
        result_tx: WorkerEventPayloadSender<CompactionPreparationTaskResult>,
        prepare: F,
    ) -> Result<(), String>
    where
        F: FnOnce() -> Result<ManualV2CompactionPreparation, String> + Send + 'static,
    {
        self.abort_all();
        let route_owner = compaction_route_owner(&session_attachment, &session_scope_id)?;
        let task_attachment = Arc::clone(&session_attachment);
        let cancelled = Arc::new(AtomicBool::new(false));
        let task_cancelled = Arc::clone(&cancelled);
        let result_session_scope_id = session_scope_id.clone();
        let handle = runtime.spawn_blocking(move || {
            let _route_owner = route_owner;
            let _session_attachment = task_attachment;
            if task_cancelled.load(Ordering::Acquire) {
                return;
            }
            let result = prepare().map(Box::new);
            if task_cancelled.load(Ordering::Acquire) {
                return;
            }
            let _ = result_tx.send(CompactionPreparationTaskResult::Manual {
                request_id,
                session_scope_id: result_session_scope_id,
                result,
            });
        });
        self.active = Some(ActiveCompactionPreparationTask {
            request_id,
            session_scope_id,
            cancelled,
            handle,
        });
        Ok(())
    }

    pub(in crate::runner) fn start_idle<F>(
        &mut self,
        runtime: &Runtime,
        request_id: u64,
        session_scope_id: String,
        session_attachment: Arc<
            sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease,
        >,
        result_tx: WorkerEventPayloadSender<CompactionPreparationTaskResult>,
        prepare: F,
    ) -> Result<(), String>
    where
        F: FnOnce() -> Result<IdleV2CompactionPreparation, String> + Send + 'static,
    {
        self.abort_all();
        let route_owner = compaction_route_owner(&session_attachment, &session_scope_id)?;
        let task_attachment = Arc::clone(&session_attachment);
        let cancelled = Arc::new(AtomicBool::new(false));
        let task_cancelled = Arc::clone(&cancelled);
        let result_session_scope_id = session_scope_id.clone();
        let handle = runtime.spawn_blocking(move || {
            let _route_owner = route_owner;
            let _session_attachment = task_attachment;
            if task_cancelled.load(Ordering::Acquire) {
                return;
            }
            let result = prepare().map(Box::new);
            if task_cancelled.load(Ordering::Acquire) {
                return;
            }
            let _ = result_tx.send(CompactionPreparationTaskResult::Idle {
                request_id,
                session_scope_id: result_session_scope_id,
                result,
            });
        });
        self.active = Some(ActiveCompactionPreparationTask {
            request_id,
            session_scope_id,
            cancelled,
            handle,
        });
        Ok(())
    }

    pub(in crate::runner) fn start_pre_turn<F>(
        &mut self,
        runtime: &Runtime,
        request_id: u64,
        session_scope_id: String,
        session_attachment: Arc<
            sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease,
        >,
        result_tx: WorkerEventPayloadSender<CompactionPreparationTaskResult>,
        prepare: F,
    ) -> Result<(), String>
    where
        F: FnOnce() -> Result<PreTurnV2CompactionPreparation, String> + Send + 'static,
    {
        self.abort_all();
        let route_owner = compaction_route_owner(&session_attachment, &session_scope_id)?;
        let task_attachment = Arc::clone(&session_attachment);
        let cancelled = Arc::new(AtomicBool::new(false));
        let task_cancelled = Arc::clone(&cancelled);
        let result_session_scope_id = session_scope_id.clone();
        let handle = runtime.spawn_blocking(move || {
            let _route_owner = route_owner;
            let _session_attachment = task_attachment;
            if task_cancelled.load(Ordering::Acquire) {
                return;
            }
            let result = prepare().map(Box::new);
            if task_cancelled.load(Ordering::Acquire) {
                return;
            }
            let _ = result_tx.send(CompactionPreparationTaskResult::PreTurn {
                request_id,
                session_scope_id: result_session_scope_id,
                result,
            });
        });
        self.active = Some(ActiveCompactionPreparationTask {
            request_id,
            session_scope_id,
            cancelled,
            handle,
        });
        Ok(())
    }

    pub(in crate::runner) fn start_overflow<F>(
        &mut self,
        runtime: &Runtime,
        request_id: u64,
        session_scope_id: String,
        session_attachment: Arc<
            sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease,
        >,
        result_tx: WorkerEventPayloadSender<CompactionPreparationTaskResult>,
        prepare: F,
    ) -> Result<(), String>
    where
        F: FnOnce() -> Result<OverflowV2CompactionPreparation, String> + Send + 'static,
    {
        self.abort_all();
        let route_owner = compaction_route_owner(&session_attachment, &session_scope_id)?;
        let task_attachment = Arc::clone(&session_attachment);
        let cancelled = Arc::new(AtomicBool::new(false));
        let task_cancelled = Arc::clone(&cancelled);
        let result_session_scope_id = session_scope_id.clone();
        let handle = runtime.spawn_blocking(move || {
            let _route_owner = route_owner;
            let _session_attachment = task_attachment;
            if task_cancelled.load(Ordering::Acquire) {
                return;
            }
            let result = prepare().map(Box::new);
            if task_cancelled.load(Ordering::Acquire) {
                return;
            }
            let _ = result_tx.send(CompactionPreparationTaskResult::Overflow {
                request_id,
                session_scope_id: result_session_scope_id,
                result,
            });
        });
        self.active = Some(ActiveCompactionPreparationTask {
            request_id,
            session_scope_id,
            cancelled,
            handle,
        });
        Ok(())
    }

    pub(in crate::runner) fn has_active(&self) -> bool {
        self.active.is_some() || self.retired.iter().any(|handle| !handle.is_finished())
    }

    pub(in crate::runner) fn reap_finished(&mut self) {
        let _ =
            super::shutdown::reap_finished_owned_tasks(&mut self.retired, &mut self.task_panicked);
    }

    pub(in crate::runner) fn accept_result(
        &mut self,
        request_id: u64,
        session_scope_id: &str,
    ) -> bool {
        if self.active.as_ref().is_some_and(|task| {
            task.request_id == request_id && task.session_scope_id == session_scope_id
        }) {
            if let Some(task) = self.active.take() {
                self.retired.push(task.handle);
            }
            self.reap_finished();
            true
        } else {
            false
        }
    }

    pub(in crate::runner) fn cancel(&mut self, request_id: u64) -> bool {
        if self
            .active
            .as_ref()
            .is_some_and(|task| task.request_id == request_id)
        {
            self.abort_all();
            true
        } else {
            false
        }
    }

    pub(in crate::runner) fn abort_all(&mut self) {
        if let Some(task) = self.active.take() {
            task.cancelled.store(true, Ordering::Release);
            task.handle.abort();
            self.retired.push(task.handle);
        }
        self.reap_finished();
    }

    pub(in crate::runner) fn shutdown_until(
        &mut self,
        runtime: &Runtime,
        deadline: std::time::Instant,
    ) -> Result<(), super::shutdown::OwnedTaskDrainFailure> {
        self.abort_all();
        super::shutdown::drain_owned_tasks_until(
            &mut self.retired,
            &mut self.task_panicked,
            runtime,
            deadline,
        )
    }

    pub(in crate::runner) fn cancel_and_join(&mut self, runtime: &Runtime) {
        self.abort_all();
        for handle in self.retired.drain(..) {
            self.task_panicked |= runtime
                .block_on(handle)
                .is_err_and(|error| error.is_panic());
        }
    }
}

fn compaction_route_owner(
    attachment: &Arc<
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease,
    >,
    session_scope_id: &str,
) -> Result<sigil_runtime::provider_connections::SessionRouteExecutionOwner, String> {
    attachment
        .route_mutation_authority(session_scope_id)
        .map_err(|error| format!("session route authority is unavailable: {error:#}"))?
        .acquire_execution_owner()
        .map_err(|error| format!("session route execution owner is unavailable: {error}"))
}

impl Drop for CompactionPreparationTaskManager {
    fn drop(&mut self) {
        self.abort_all();
    }
}

#[cfg(test)]
mod shutdown_tests {
    use super::*;

    #[test]
    fn finished_compaction_panic_survives_abort_and_repeated_shutdown() {
        let runtime = Runtime::new().expect("build compaction shutdown test runtime");
        let handle = runtime.spawn(async { panic!("compaction fixture panic") });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !handle.is_finished() && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(handle.is_finished());
        let mut manager = CompactionPreparationTaskManager::new();
        manager.active = Some(ActiveCompactionPreparationTask {
            request_id: 1,
            session_scope_id: "panic-scope".to_owned(),
            cancelled: Arc::new(AtomicBool::new(false)),
            handle,
        });
        manager.abort_all();
        assert!(manager.retired.is_empty());
        assert!(manager.task_panicked);
        for _ in 0..2 {
            assert!(matches!(
                manager.shutdown_until(&runtime, deadline),
                Err(super::super::shutdown::OwnedTaskDrainFailure::TaskPanicked)
            ));
        }
    }
}
