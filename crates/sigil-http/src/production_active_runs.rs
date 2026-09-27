use std::{
    collections::BTreeMap,
    sync::{Arc, Condvar, LockResult, Mutex, MutexGuard, WaitTimeoutResult},
    time::Duration,
};

use tokio::sync::Notify;

use super::HttpProductionActiveRun;

/// Shares run-release notifications between blocking shutdown callers and async observers.
#[derive(Default)]
pub(super) struct HttpActiveRunsReady {
    blocking: Condvar,
    asynchronous: Notify,
}

impl HttpActiveRunsReady {
    pub(super) fn notify_all(&self) {
        self.blocking.notify_all();
        self.asynchronous.notify_waiters();
    }

    pub(super) fn wait_timeout<'a, T>(
        &self,
        guard: MutexGuard<'a, T>,
        timeout: Duration,
    ) -> LockResult<(MutexGuard<'a, T>, WaitTimeoutResult)> {
        self.blocking.wait_timeout(guard, timeout)
    }

    /// Observes actual owner release without retaining an uncancellable blocking worker.
    pub(super) async fn wait_for_session_idle(
        &self,
        active_runs: &Mutex<BTreeMap<String, Arc<HttpProductionActiveRun>>>,
        session_id: &str,
    ) -> anyhow::Result<()> {
        loop {
            let notified = self.asynchronous.notified();
            tokio::pin!(notified);
            // Register before reading the map so release between the read and await is retained.
            notified.as_mut().enable();
            let active = active_runs
                .lock()
                .map_err(|_| anyhow::anyhow!("HTTP active-run state poisoned"))?
                .values()
                .any(|run| run.session_id == session_id);
            if !active {
                return Ok(());
            }
            notified.await;
        }
    }
}
