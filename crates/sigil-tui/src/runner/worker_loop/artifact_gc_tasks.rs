use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use sigil_kernel::{ProjectionCursor, SessionRef, ToolArtifactGcReportV1, ToolArtifactGcRootsV1};
use sigil_runtime::{LocalSessionLifecycleService, current_unix_time_ms};
use tokio::{runtime::Runtime, task::JoinHandle};

use crate::runner::worker_event::WorkerEventPayloadSender;

static ARTIFACT_GC_TASK_STARTED_TOTAL: AtomicU64 = AtomicU64::new(0);
static ARTIFACT_GC_TASK_COMPLETED_TOTAL: AtomicU64 = AtomicU64::new(0);

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ArtifactGcTaskMetricsSnapshot {
    pub started_total: u64,
    pub completed_total: u64,
}

#[cfg_attr(not(test), allow(dead_code))]
impl ArtifactGcTaskMetricsSnapshot {
    #[must_use]
    pub fn saturating_delta(self, earlier: Self) -> Self {
        Self {
            started_total: self.started_total.saturating_sub(earlier.started_total),
            completed_total: self.completed_total.saturating_sub(earlier.completed_total),
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
#[must_use]
pub fn artifact_gc_task_metrics() -> ArtifactGcTaskMetricsSnapshot {
    ArtifactGcTaskMetricsSnapshot {
        started_total: ARTIFACT_GC_TASK_STARTED_TOTAL.load(Ordering::Relaxed),
        completed_total: ARTIFACT_GC_TASK_COMPLETED_TOTAL.load(Ordering::Relaxed),
    }
}

pub(in crate::runner) struct ArtifactGcTaskResult {
    pub(in crate::runner) request_id: u64,
    pub(in crate::runner) session_scope_id: String,
    pub(in crate::runner) projection_cursor: Option<ProjectionCursor>,
    pub(in crate::runner) result: Result<ToolArtifactGcReportV1, String>,
}

#[derive(Default)]
pub(in crate::runner) struct ArtifactGcTaskManager {
    active: Option<ActiveArtifactGcTask>,
    retired: Vec<JoinHandle<()>>,
}

struct ActiveArtifactGcTask {
    request_id: u64,
    session_scope_id: String,
    cancelled: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

impl ArtifactGcTaskManager {
    pub(in crate::runner) fn new() -> Self {
        Self::default()
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::runner) fn start(
        &mut self,
        runtime: &Runtime,
        request_id: u64,
        session_scope_id: String,
        session_attachment: Arc<
            sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease,
        >,
        projection_cursor: Option<ProjectionCursor>,
        result_tx: WorkerEventPayloadSender<ArtifactGcTaskResult>,
        lifecycle: LocalSessionLifecycleService,
        session_ref: SessionRef,
        roots: ToolArtifactGcRootsV1,
    ) {
        debug_assert!(self.active.is_none());
        let cancelled = Arc::new(AtomicBool::new(false));
        let task_cancelled = Arc::clone(&cancelled);
        let task_attachment = Arc::clone(&session_attachment);
        let result_session_scope_id = session_scope_id.clone();
        ARTIFACT_GC_TASK_STARTED_TOTAL.fetch_add(1, Ordering::Relaxed);
        let handle = runtime.spawn_blocking(move || {
            let _session_attachment = task_attachment;
            if task_cancelled.load(Ordering::Acquire) {
                return;
            }
            let result = lifecycle
                .garbage_collect_session_artifacts(
                    &session_ref,
                    &result_session_scope_id,
                    roots,
                    current_unix_time_ms(),
                )
                .map_err(|error| format!("{error:#}"));
            ARTIFACT_GC_TASK_COMPLETED_TOTAL.fetch_add(1, Ordering::Relaxed);
            if task_cancelled.load(Ordering::Acquire) {
                return;
            }
            let _ = result_tx.send(ArtifactGcTaskResult {
                request_id,
                session_scope_id: result_session_scope_id,
                projection_cursor,
                result,
            });
        });
        self.active = Some(ActiveArtifactGcTask {
            request_id,
            session_scope_id,
            cancelled,
            handle,
        });
    }

    pub(in crate::runner) fn has_active(&self) -> bool {
        // A task stops owning the GC conflict gate when its result is accepted. Its join handle
        // can remain briefly unfinished while the spawn-blocking closure unwinds after sending
        // that result; treating such retired handles as active can strand an ordinary command at
        // the head of the worker queue with no later event left to wake the reactor.
        self.active.is_some()
    }

    pub(in crate::runner) fn reap_finished(&mut self) {
        self.retired.retain(|handle| !handle.is_finished());
    }

    pub(in crate::runner) fn accept_result(
        &mut self,
        request_id: u64,
        session_scope_id: &str,
    ) -> bool {
        if self.active.as_ref().is_some_and(|task| {
            task.request_id == request_id && task.session_scope_id == session_scope_id
        }) {
            if let Some(task) = self.active.take()
                && !task.handle.is_finished()
            {
                self.retired.push(task.handle);
            }
            self.reap_finished();
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

    pub(in crate::runner) fn cancel_and_join(&mut self, runtime: &Runtime) {
        self.abort_all();
        for handle in self.retired.drain(..) {
            let _ = runtime.block_on(handle);
        }
    }
}

impl Drop for ArtifactGcTaskManager {
    fn drop(&mut self) {
        self.abort_all();
    }
}

#[cfg(test)]
mod tests {
    use std::future;

    use super::*;

    #[test]
    fn retired_result_handle_does_not_keep_gc_conflict_gate_active() {
        let runtime = Runtime::new().expect("test runtime");
        let handle = runtime.spawn(async { future::pending::<()>().await });
        assert!(!handle.is_finished());
        let mut tasks = ArtifactGcTaskManager {
            active: None,
            retired: vec![handle],
        };

        assert!(!tasks.has_active());

        for handle in &tasks.retired {
            handle.abort();
        }
        tasks.abort_all();
        tasks.cancel_and_join(&runtime);
    }
}
