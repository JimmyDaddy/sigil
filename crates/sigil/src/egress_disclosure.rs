use std::{
    io::{self, Write},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use sigil_kernel::{
    DisclosurePresentationError, DisclosurePresentationReceipt, EgressDataCategory,
    EgressDisclosurePresenter, EgressNetworkRoute, PreEgressDisclosure,
};

/// Concrete CLI disclosure presenter that keeps machine-readable stdout untouched.
#[derive(Clone)]
pub struct CliEgressDisclosurePresenter {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    sink_fingerprint: &'static str,
}

impl std::fmt::Debug for CliEgressDisclosurePresenter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CliEgressDisclosurePresenter")
            .field("sink_fingerprint", &self.sink_fingerprint)
            .finish_non_exhaustive()
    }
}

impl CliEgressDisclosurePresenter {
    /// Creates a presenter that writes safe disclosures to standard error.
    #[must_use]
    pub fn stderr() -> Self {
        Self::with_writer(io::stderr())
    }

    /// Creates a presenter from an injected stderr-compatible sink.
    #[must_use]
    pub fn with_writer<W>(writer: W) -> Self
    where
        W: Write + Send + 'static,
    {
        Self {
            writer: Arc::new(Mutex::new(Box::new(writer))),
            sink_fingerprint: "cli-stderr-flush-v1",
        }
    }
}

#[async_trait]
impl EgressDisclosurePresenter for CliEgressDisclosurePresenter {
    async fn present(
        &self,
        disclosure: PreEgressDisclosure,
    ) -> Result<DisclosurePresentationReceipt, DisclosurePresentationError> {
        let writer = self.writer.clone();
        let sink_fingerprint = self.sink_fingerprint;
        tokio::task::spawn_blocking(move || {
            let message = format_disclosure(&disclosure);
            let mut writer = writer
                .lock()
                .map_err(|_| DisclosurePresentationError::SinkClosed)?;
            writer
                .write_all(message.as_bytes())
                .map_err(|_| DisclosurePresentationError::WriteFailed)?;
            writer
                .flush()
                .map_err(|_| DisclosurePresentationError::FlushFailed)?;
            disclosure.presentation_receipt(sink_fingerprint)
        })
        .await
        .map_err(|_| DisclosurePresentationError::SinkClosed)?
    }
}

fn format_disclosure(disclosure: &PreEgressDisclosure) -> String {
    format!(
        "[sigil network disclosure]\n{}\nsurface: {}\ndestination: {}\nroute: {}\ndata: {}\n",
        disclosure.display_name(),
        disclosure.surface(),
        disclosure.safe_logical_destination(),
        match disclosure.route() {
            EgressNetworkRoute::Direct => "direct",
            EgressNetworkRoute::ProxyRemote => "environment proxy",
        },
        disclosure
            .data_categories()
            .iter()
            .map(|category| match category {
                EgressDataCategory::SearchQuery => "search query",
                EgressDataCategory::ConnectionMetadata => "connection metadata",
                EgressDataCategory::WorkspaceRootUri => "workspace root URI",
                EgressDataCategory::InteractiveUserResponse => "interactive response",
            })
            .collect::<Vec<_>>()
            .join(", "),
    )
}

#[cfg(test)]
#[path = "tests/egress_disclosure_tests.rs"]
mod tests;
