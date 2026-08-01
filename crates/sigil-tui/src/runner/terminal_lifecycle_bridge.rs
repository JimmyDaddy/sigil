use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use sigil_kernel::{
    MutationEventRecorder, TerminalLifecycleSink, TerminalLifecycleSinkFactory,
    TerminalLifecycleUpdateV2,
};

use super::worker_event::{
    RoutedTerminalLifecycleUpdate, WorkerEvent, WorkerTerminalLifecycleEventSender,
};

/// TUI lifecycle factory that freezes the exact immutable session/run route at `terminal_start`.
#[derive(Clone)]
pub(in crate::runner) struct ChannelTerminalLifecycleRouter {
    sender: WorkerTerminalLifecycleEventSender,
}

impl ChannelTerminalLifecycleRouter {
    pub(in crate::runner) fn new(event_tx: std::sync::mpsc::Sender<WorkerEvent>) -> Self {
        Self {
            sender: WorkerTerminalLifecycleEventSender::new(event_tx),
        }
    }
}

impl std::fmt::Debug for ChannelTerminalLifecycleRouter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChannelTerminalLifecycleRouter")
            .finish_non_exhaustive()
    }
}

impl TerminalLifecycleSinkFactory for ChannelTerminalLifecycleRouter {
    fn sink_for_run(
        &self,
        session_scope_id: &str,
        logical_run_id: &str,
        recorder: MutationEventRecorder,
    ) -> Result<Arc<dyn TerminalLifecycleSink>> {
        Ok(Arc::new(BoundTerminalLifecycleRoute {
            session_scope_id: session_scope_id.to_owned(),
            logical_run_id: logical_run_id.to_owned(),
            recorder,
            sender: self.sender.clone(),
        }))
    }
}

#[derive(Debug)]
struct BoundTerminalLifecycleRoute {
    session_scope_id: String,
    logical_run_id: String,
    recorder: MutationEventRecorder,
    sender: WorkerTerminalLifecycleEventSender,
}

#[async_trait]
impl TerminalLifecycleSink for BoundTerminalLifecycleRoute {
    async fn publish(&self, update: TerminalLifecycleUpdateV2) -> Result<()> {
        TerminalLifecycleSink::publish(&self.recorder, update.clone()).await?;
        self.sender
            .send(RoutedTerminalLifecycleUpdate {
                session_scope_id: self.session_scope_id.clone(),
                run_id: self.logical_run_id.clone(),
                update,
            })
            .map_err(anyhow::Error::new)
            .context("failed to wake the TUI terminal lifecycle worker")
    }
}

#[cfg(test)]
#[path = "tests/terminal_lifecycle_bridge_tests.rs"]
mod tests;
