use std::{collections::BTreeMap, path::PathBuf, time::Duration};

use anyhow::Result;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};

/// Stable identifier for an execution backend implementation.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionBackendKind {
    #[default]
    Local,
    MacosSeatbelt,
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

impl ExecutionBackendCapabilities {
    /// Returns whether the backend can enforce a basic OS-level sandbox boundary.
    #[must_use]
    pub fn supports_required_sandbox(self) -> bool {
        self.filesystem_isolation && self.process_isolation
    }
}

/// User-configurable execution policy.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ExecutionConfig {
    #[serde(default)]
    pub backend: ExecutionBackendKind,
    #[serde(default)]
    pub isolation: ExecutionIsolationPolicy,
}

/// Required isolation level for command execution.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionIsolationPolicy {
    /// Preserve current local process behavior. This is not a sandbox.
    #[default]
    AllowLocal,
    /// Require a backend that can enforce filesystem and process isolation.
    RequireSandbox,
}

impl ExecutionIsolationPolicy {
    #[must_use]
    pub fn requires_sandbox(self) -> bool {
        matches!(self, Self::RequireSandbox)
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    pub timeout_secs: u64,
}

impl ExecutionRequest {
    /// Returns the effective timeout for this request.
    ///
    /// Millisecond precision is used when supplied. A zero second timeout with no millisecond
    /// override means the caller intentionally requested no backend timeout.
    #[must_use]
    pub fn timeout_duration(&self) -> Option<Duration> {
        if let Some(timeout_ms) = self.timeout_ms {
            return Some(Duration::from_millis(timeout_ms));
        }
        (self.timeout_secs > 0).then(|| Duration::from_secs(self.timeout_secs))
    }
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
