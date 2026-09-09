use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use sigil_kernel::{
    MutationEventRecorder, PublicRunEvent, TerminalLifecycleSink, TerminalLifecycleUpdateV2,
};

use crate::application_run::{ApplicationRunEventHandler, ApplicationRunEventSequence};

/// Adapter-owned bounded projection for terminal lifecycle events.
pub trait ApplicationTerminalLifecycleHandler: Send + Sync + std::fmt::Debug {
    /// Projects one exact durable public event to the active product surface.
    ///
    /// The runtime assigns this event's sequence and persists it in the session outbox before
    /// invoking the adapter. The adapter must not renumber or synthesize a replacement event.
    fn handle_public_event(&self, event: PublicRunEvent) -> Result<()>;

    /// Bounded durable adapter identity used by the public outbox receipt.
    fn public_event_adapter_id(&self) -> &'static str;
}

#[derive(Debug)]
struct TerminalLifecyclePublicEventHandler {
    handler: Arc<dyn ApplicationTerminalLifecycleHandler>,
}

impl ApplicationRunEventHandler for TerminalLifecyclePublicEventHandler {
    fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
        self.handler.handle_public_event(event)
    }

    fn public_event_adapter_id(&self) -> &'static str {
        self.handler.public_event_adapter_id()
    }
}

/// Session/run-bound router that persists exact owner state before live publication.
#[derive(Debug)]
pub struct ApplicationTerminalLifecycleRouter {
    recorder: MutationEventRecorder,
    publication: TerminalLifecyclePublication,
}

#[derive(Debug)]
enum TerminalLifecyclePublication {
    DomainOnly,
    ApplicationPublic {
        handler: Arc<dyn ApplicationTerminalLifecycleHandler>,
        events: Box<ApplicationRunEventSequence>,
    },
}

impl ApplicationTerminalLifecycleRouter {
    /// Creates a domain-only lifecycle route with no public adapter publication.
    #[must_use]
    pub fn new(recorder: MutationEventRecorder) -> Self {
        Self {
            recorder,
            publication: TerminalLifecyclePublication::DomainOnly,
        }
    }

    /// Binds one adapter and the run's sole durable public-outbox sequence as one publication
    /// route. There is deliberately no public-handler-only construction path.
    #[must_use]
    pub(crate) fn with_application_public_events(
        mut self,
        handler: Arc<dyn ApplicationTerminalLifecycleHandler>,
        events: ApplicationRunEventSequence,
    ) -> Self {
        self.publication = TerminalLifecyclePublication::ApplicationPublic {
            handler,
            events: Box::new(events),
        };
        self
    }
}

#[async_trait]
impl TerminalLifecycleSink for ApplicationTerminalLifecycleRouter {
    async fn publish(&self, update: TerminalLifecycleUpdateV2) -> Result<()> {
        let event = update.event.clone();
        TerminalLifecycleSink::publish(&self.recorder, update).await?;
        if let TerminalLifecyclePublication::ApplicationPublic { handler, events } =
            &self.publication
        {
            let mut public_handler = TerminalLifecyclePublicEventHandler {
                handler: Arc::clone(handler),
            };
            // Delivery and receipt errors become outbox degradation inside the sequence. Only an
            // inability to persist the exact lifecycle public event is returned to the owner.
            events.emit_terminal_lifecycle(&mut public_handler, event)?;
        }
        Ok(())
    }
}
