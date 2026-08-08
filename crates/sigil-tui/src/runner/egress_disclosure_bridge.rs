use std::sync::mpsc;

use anyhow::Context;
use async_trait::async_trait;
use sigil_kernel::{
    DisclosurePresentationError, DisclosurePresentationReceipt, EgressDisclosurePresenter,
    PreEgressDisclosure,
};
use tokio::sync::oneshot;

use super::protocol::WorkerMessage;

/// Worker-side TUI presenter that waits for a successful terminal frame before acknowledging.
#[derive(Debug, Clone)]
pub struct ChannelEgressDisclosurePresenter {
    message_tx: mpsc::Sender<WorkerMessage>,
}

/// Auto-accepts egress disclosures without a modal.
///
/// Used when the configured network policy already authorizes the route (Allow/Deny): the
/// disclosure record is still appended durably by the egress ordering layer, only the interactive
/// confirmation is skipped. `Ask` keeps the interactive presenter.
#[derive(Debug, Clone, Copy)]
pub struct AutoAcceptDisclosurePresenter;

#[async_trait]
impl EgressDisclosurePresenter for AutoAcceptDisclosurePresenter {
    async fn present(
        &self,
        disclosure: PreEgressDisclosure,
    ) -> Result<DisclosurePresentationReceipt, DisclosurePresentationError> {
        disclosure.presentation_receipt("tui-auto-accept-v1")
    }
}

impl ChannelEgressDisclosurePresenter {
    /// Creates a presenter that requests a frame-render acknowledgement from the TUI worker UI.
    #[must_use]
    pub fn new(message_tx: mpsc::Sender<WorkerMessage>) -> Self {
        Self { message_tx }
    }
}

#[async_trait]
impl EgressDisclosurePresenter for ChannelEgressDisclosurePresenter {
    async fn present(
        &self,
        disclosure: PreEgressDisclosure,
    ) -> Result<DisclosurePresentationReceipt, DisclosurePresentationError> {
        let (receipt_tx, receipt_rx) = oneshot::channel();
        self.message_tx
            .send(WorkerMessage::EgressDisclosureRequested {
                disclosure,
                receipt_tx,
            })
            .context("failed to send egress disclosure request to TUI")
            .map_err(|_| DisclosurePresentationError::SinkClosed)?;
        receipt_rx
            .await
            .map_err(|_| DisclosurePresentationError::SinkClosed)?
    }
}

#[cfg(test)]
#[path = "tests/egress_disclosure_bridge_tests.rs"]
mod tests;
