//! Host-private process lifecycle port for language servers.

use std::{path::PathBuf, sync::Arc};

use anyhow::Result;
use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncWrite};

pub type LanguageServerReaderV1 = Box<dyn AsyncRead + Send + Unpin>;
pub type LanguageServerWriterV1 = Box<dyn AsyncWrite + Send + Unpin>;
pub type LanguageServerShutdownV1 = Arc<dyn Fn() + Send + Sync>;

/// Resolved language-server launch material supplied by the runtime authority route.
#[derive(Debug, Clone)]
pub struct LanguageServerLaunchRequestV1 {
    /// Stable configured server name used for admission and diagnostics.
    pub server_name: String,
    /// Resolved executable path. The code-intel crate does not resolve or spawn it.
    pub program: PathBuf,
    /// Resolved executable arguments.
    pub args: Vec<String>,
    /// Authority-bound workspace root used as the process cwd.
    pub cwd: PathBuf,
    /// Sanitized, planner-bound environment entries.
    pub environment: Vec<(String, String)>,
}

/// Managed language-server stdio transport returned by a runtime launch port.
pub struct LanguageServerProcessIoV1 {
    reader: LanguageServerReaderV1,
    writer: LanguageServerWriterV1,
    shutdown: LanguageServerShutdownV1,
}

impl LanguageServerProcessIoV1 {
    /// Creates a transport from a managed bidirectional stdio bridge.
    pub fn new<R, W, F>(reader: R, writer: W, shutdown: F) -> Self
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
        F: Fn() + Send + Sync + 'static,
    {
        Self {
            reader: Box::new(reader),
            writer: Box::new(writer),
            shutdown: Arc::new(shutdown),
        }
    }

    /// Splits the transport into its async stdio halves and synchronous shutdown signal.
    pub fn into_parts(
        self,
    ) -> (
        LanguageServerReaderV1,
        LanguageServerWriterV1,
        LanguageServerShutdownV1,
    ) {
        (self.reader, self.writer, self.shutdown)
    }
}

/// Runtime-owned launch seam for long-lived language-server processes.
#[async_trait]
pub trait LanguageServerLaunchPortV1: Send + Sync {
    /// Launches one authority-bound language server and returns its stdio bridge.
    async fn launch(
        &self,
        request: LanguageServerLaunchRequestV1,
    ) -> Result<LanguageServerProcessIoV1>;
}
