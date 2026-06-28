use std::{collections::BTreeMap, path::PathBuf};

use anyhow::Result;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};

/// Stable identifier for an execution backend implementation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionBackendKind {
    Local,
}

/// Capability summary for an execution backend.
///
/// These flags describe what the backend can enforce. They are intentionally separate from
/// permission policy: policy decides whether execution is allowed, while backend capabilities
/// describe what is actually isolated once execution starts.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ExecutionBackendCapabilities {
    pub filesystem_isolation: bool,
    pub network_isolation: bool,
    pub process_isolation: bool,
    pub resource_limits: bool,
    pub persistent_pty: bool,
    pub workspace_snapshot: bool,
}

/// One non-interactive process execution request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ExecutionRequest {
    pub program: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    pub timeout_secs: u64,
}

/// Result captured by an execution backend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ExecutionReceipt {
    pub backend: ExecutionBackendKind,
    pub capabilities: ExecutionBackendCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub stdout: Vec<u8>,
    #[serde(default)]
    pub stderr: Vec<u8>,
    pub timed_out: bool,
}

/// Execution backend for non-interactive commands.
pub type ExecutionFuture<'a> = BoxFuture<'a, Result<ExecutionReceipt>>;

pub trait ExecutionBackend: Send + Sync {
    fn kind(&self) -> ExecutionBackendKind;

    fn capabilities(&self) -> ExecutionBackendCapabilities;

    /// Executes one non-interactive command.
    ///
    /// # Errors
    ///
    /// Returns an error when process spawning, waiting, or output collection fails. Timeouts are
    /// represented as successful receipts with `timed_out = true`, so callers can map them into
    /// structured tool errors without losing backend metadata.
    fn execute(&self, request: ExecutionRequest) -> ExecutionFuture<'_>;
}
