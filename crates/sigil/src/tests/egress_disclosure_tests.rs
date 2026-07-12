use std::{
    io::{self, Write},
    sync::{Arc, Mutex},
};

use sigil_kernel::{
    DisclosurePresentationError, EgressDataCategory, EgressDisclosureKind,
    EgressDisclosurePresenter, EgressNetworkRoute, PreEgressDisclosure,
};

use super::CliEgressDisclosurePresenter;

#[derive(Clone)]
struct RecordingWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
    fail_write: bool,
    fail_flush: bool,
}

impl Write for RecordingWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if self.fail_write {
            return Err(io::Error::other("write failed"));
        }
        self.bytes
            .lock()
            .expect("test writer lock")
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.fail_flush {
            return Err(io::Error::other("flush failed"));
        }
        Ok(())
    }
}

fn disclosure() -> PreEgressDisclosure {
    PreEgressDisclosure::new(
        EgressDisclosureKind::Query,
        Some("query-1".to_owned()),
        "exa-anonymous-2026-06-29",
        "cli",
        "Exa no-key free tier",
        "route-fingerprint",
        "profile-fingerprint",
        "https://mcp.exa.ai/",
        "https://mcp.exa.ai/",
        EgressNetworkRoute::Direct,
        vec![EgressDataCategory::SearchQuery],
    )
    .expect("valid safe disclosure")
}

#[tokio::test]
async fn presenter_writes_safe_disclosure_to_stderr_sink_and_flushes_before_receipt() {
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let presenter = CliEgressDisclosurePresenter::with_writer(RecordingWriter {
        bytes: bytes.clone(),
        fail_write: false,
        fail_flush: false,
    });

    let receipt = presenter
        .present(disclosure())
        .await
        .expect("flushed stderr sink should acknowledge");
    let output = String::from_utf8(bytes.lock().expect("test writer lock").clone())
        .expect("utf8 test output");

    assert!(output.contains("[sigil network disclosure]"));
    assert!(output.contains("destination: https://mcp.exa.ai/"));
    assert!(output.contains("data: search query"));
    assert_eq!(receipt.correlation_id(), Some("query-1"));
    assert_eq!(receipt.sink_fingerprint(), "cli-stderr-flush-v1");
}

#[tokio::test]
async fn write_failure_never_returns_a_receipt() {
    let presenter = CliEgressDisclosurePresenter::with_writer(RecordingWriter {
        bytes: Arc::new(Mutex::new(Vec::new())),
        fail_write: true,
        fail_flush: false,
    });

    assert!(matches!(
        presenter.present(disclosure()).await,
        Err(DisclosurePresentationError::WriteFailed)
    ));
}

#[tokio::test]
async fn flush_failure_never_returns_a_receipt() {
    let presenter = CliEgressDisclosurePresenter::with_writer(RecordingWriter {
        bytes: Arc::new(Mutex::new(Vec::new())),
        fail_write: false,
        fail_flush: true,
    });

    assert!(matches!(
        presenter.present(disclosure()).await,
        Err(DisclosurePresentationError::FlushFailed)
    ));
}
