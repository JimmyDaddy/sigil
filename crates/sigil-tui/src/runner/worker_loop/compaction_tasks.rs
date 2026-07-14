use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

use tokio::{runtime::Runtime, task::JoinHandle};

use super::PendingV2Compaction;
use crate::runner::V2CompactionReview;

pub(in crate::runner) struct ManualV2CompactionPreparation {
    pub(in crate::runner) review: V2CompactionReview,
    pub(in crate::runner) pending: Option<PendingV2Compaction>,
}

pub(in crate::runner) enum CompactionPreparationTaskResult {
    Manual {
        request_id: u64,
        session_scope_id: String,
        result: Result<ManualV2CompactionPreparation, String>,
    },
}

#[derive(Default)]
pub(in crate::runner) struct CompactionPreparationTaskManager {
    active: Option<ActiveCompactionPreparationTask>,
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
        result_tx: mpsc::Sender<CompactionPreparationTaskResult>,
        prepare: F,
    ) where
        F: FnOnce() -> Result<ManualV2CompactionPreparation, String> + Send + 'static,
    {
        self.abort_all();
        let cancelled = Arc::new(AtomicBool::new(false));
        let task_cancelled = Arc::clone(&cancelled);
        let result_session_scope_id = session_scope_id.clone();
        let handle = runtime.spawn_blocking(move || {
            if task_cancelled.load(Ordering::Acquire) {
                return;
            }
            let result = prepare();
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
    }

    pub(in crate::runner) fn accept_result(
        &mut self,
        request_id: u64,
        session_scope_id: &str,
    ) -> bool {
        if self.active.as_ref().is_some_and(|task| {
            task.request_id == request_id && task.session_scope_id == session_scope_id
        }) {
            self.active = None;
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
        }
    }
}

impl Drop for CompactionPreparationTaskManager {
    fn drop(&mut self) {
        self.abort_all();
    }
}
