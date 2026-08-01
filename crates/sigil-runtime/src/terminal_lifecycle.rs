use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use sigil_kernel::{
    MutationEventRecorder, TerminalLifecycleEvent, TerminalLifecycleSink, TerminalLifecycleUpdateV2,
};

/// Adapter-owned bounded projection for terminal lifecycle events.
pub trait ApplicationTerminalLifecycleHandler: Send + Sync + std::fmt::Debug {
    /// Publishes one already-durable lifecycle event to the active product surface.
    fn handle_terminal_lifecycle(
        &self,
        session_id: &str,
        run_id: &str,
        event: &TerminalLifecycleEvent,
    ) -> Result<()>;
}

/// Session/run-bound router that persists exact owner state before live publication.
#[derive(Debug)]
pub struct ApplicationTerminalLifecycleRouter {
    recorder: MutationEventRecorder,
    session_id: String,
    run_id: String,
    handler: Option<Arc<dyn ApplicationTerminalLifecycleHandler>>,
}

impl ApplicationTerminalLifecycleRouter {
    #[must_use]
    pub fn new(
        recorder: MutationEventRecorder,
        session_id: impl Into<String>,
        run_id: impl Into<String>,
        handler: Option<Arc<dyn ApplicationTerminalLifecycleHandler>>,
    ) -> Self {
        Self {
            recorder,
            session_id: session_id.into(),
            run_id: run_id.into(),
            handler,
        }
    }
}

#[async_trait]
impl TerminalLifecycleSink for ApplicationTerminalLifecycleRouter {
    async fn publish(&self, update: TerminalLifecycleUpdateV2) -> Result<()> {
        let event = update.event.clone();
        TerminalLifecycleSink::publish(&self.recorder, update).await?;
        if let Some(handler) = &self.handler {
            handler.handle_terminal_lifecycle(&self.session_id, &self.run_id, &event)?;
        }
        Ok(())
    }
}
