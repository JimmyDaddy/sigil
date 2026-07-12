use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sigil_kernel::{
    DisclosurePresentationError, DisclosurePresentationReceipt, EgressDisclosurePresenter,
    PreEgressDisclosure,
};
use thiserror::Error;

/// Schema version for synthetic HTTP disclosure replay events.
pub const HTTP_EGRESS_DISCLOSURE_SCHEMA_VERSION: u32 = 1;

/// A dedicated structured disclosure event retained by the synthetic HTTP replay adapter.
///
/// This event only proves that the safe disclosure entered a server-side replay buffer. It neither
/// starts a listener nor proves that a remote subscriber or person observed it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct HttpEgressDisclosureEvent {
    /// Schema version for this dedicated replay record.
    pub schema_version: u32,
    /// Stable event discriminator kept separate from public run events.
    pub event_type: String,
    /// Safe disclosure fields needed by a future replay surface.
    pub disclosure: PreEgressDisclosure,
}

impl HttpEgressDisclosureEvent {
    fn from_disclosure(disclosure: PreEgressDisclosure) -> Self {
        Self {
            schema_version: HTTP_EGRESS_DISCLOSURE_SCHEMA_VERSION,
            event_type: "egress_disclosure".to_owned(),
            disclosure,
        }
    }
}

#[derive(Debug, Default)]
struct ReplayState {
    events: Vec<HttpEgressDisclosureEvent>,
    closed: bool,
    fail_next_publish: bool,
}

/// In-memory server-side replay buffer for the HTTP synthetic presenter contract.
#[derive(Debug, Default)]
pub struct HttpEgressDisclosureReplayBuffer {
    state: Mutex<ReplayState>,
}

impl HttpEgressDisclosureReplayBuffer {
    /// Creates an empty synthetic disclosure replay buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Publishes one structured disclosure event before the presenter acknowledges it.
    pub fn publish(
        &self,
        disclosure: PreEgressDisclosure,
    ) -> Result<(), HttpEgressDisclosureReplayError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| HttpEgressDisclosureReplayError::Unavailable)?;
        if state.closed {
            return Err(HttpEgressDisclosureReplayError::Closed);
        }
        if std::mem::take(&mut state.fail_next_publish) {
            return Err(HttpEgressDisclosureReplayError::PublishFailed);
        }
        state
            .events
            .push(HttpEgressDisclosureEvent::from_disclosure(disclosure));
        Ok(())
    }

    /// Returns a bounded snapshot for synthetic replay assertions and future adapter wiring.
    #[must_use]
    pub fn events(&self) -> Vec<HttpEgressDisclosureEvent> {
        self.state
            .lock()
            .map(|state| state.events.clone())
            .unwrap_or_default()
    }

    /// Closes the synthetic sink so subsequent presentation fails closed.
    pub fn close(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.closed = true;
        }
    }

    #[cfg(test)]
    pub(crate) fn fail_next_publish(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.fail_next_publish = true;
        }
    }
}

/// Errors from the synthetic replay buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum HttpEgressDisclosureReplayError {
    /// The replay sink was closed before publication.
    #[error("http disclosure replay buffer is closed")]
    Closed,
    /// The replay sink rejected this event.
    #[error("http disclosure replay publish failed")]
    PublishFailed,
    /// The replay buffer state is unavailable.
    #[error("http disclosure replay buffer is unavailable")]
    Unavailable,
}

/// Concrete synthetic HTTP presenter used until a real HTTP product surface is separately wired.
#[derive(Clone, Debug)]
pub struct HttpReplayEgressDisclosurePresenter {
    replay: Arc<HttpEgressDisclosureReplayBuffer>,
    sink_fingerprint: &'static str,
}

impl HttpReplayEgressDisclosurePresenter {
    /// Creates a presenter that acknowledges only after replay-buffer publication succeeds.
    #[must_use]
    pub fn new(replay: Arc<HttpEgressDisclosureReplayBuffer>) -> Self {
        Self {
            replay,
            sink_fingerprint: "http-synthetic-replay-buffer-v1",
        }
    }
}

#[async_trait]
impl EgressDisclosurePresenter for HttpReplayEgressDisclosurePresenter {
    async fn present(
        &self,
        disclosure: PreEgressDisclosure,
    ) -> Result<DisclosurePresentationReceipt, DisclosurePresentationError> {
        self.replay
            .publish(disclosure.clone())
            .map_err(|error| match error {
                HttpEgressDisclosureReplayError::Closed => DisclosurePresentationError::SinkClosed,
                HttpEgressDisclosureReplayError::PublishFailed
                | HttpEgressDisclosureReplayError::Unavailable => {
                    DisclosurePresentationError::WriteFailed
                }
            })?;
        disclosure.presentation_receipt(self.sink_fingerprint)
    }
}

#[cfg(test)]
#[path = "tests/disclosure_tests.rs"]
mod tests;
