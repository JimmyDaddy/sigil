use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CodeIntelError {
    #[error("code intelligence is disabled")]
    Disabled,
    #[error("no language server configured for {path}")]
    NoServerForPath { path: String },
    #[error("language server {server} is unavailable: {reason}")]
    ServerUnavailable { server: String, reason: String },
    #[error("workspace trust is required before starting language server {server}")]
    WorkspaceTrustRequired { server: String },
    #[error("language server {server} does not support {capability}")]
    UnsupportedCapability {
        server: String,
        capability: &'static str,
    },
    #[error("path is outside workspace: {path}")]
    PathOutsideWorkspace { path: String },
    #[error("path does not exist: {path}")]
    NotFound { path: String },
    #[error("failed to parse language server message: {reason}")]
    Protocol { reason: String },
    #[error("language server request timed out: {operation}")]
    Timeout { operation: String },
    #[error("failed to read {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}
