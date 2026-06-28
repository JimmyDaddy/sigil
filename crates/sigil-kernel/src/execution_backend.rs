use std::{collections::BTreeMap, path::PathBuf, time::Duration};

use anyhow::Result;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};

use crate::tool::ToolCategory;

/// Stable identifier for an execution backend implementation.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionBackendKind {
    #[default]
    Local,
    MacosSeatbelt,
}

impl ExecutionBackendKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::MacosSeatbelt => "macos_seatbelt",
        }
    }
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
    #[serde(default)]
    pub profile: ExecutionSandboxProfile,
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

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionSandboxProfile {
    /// Preserve current execution behavior. This is not a sandbox profile.
    #[default]
    Unconfined,
    /// Commands may write the workspace but should not mutate user state outside it.
    WorkspaceWrite,
    /// Build-like commands may read dependency caches but must not use the network.
    BuildOffline,
    /// Build-like commands may read dependency caches and use the network.
    BuildNetworked,
}

impl ExecutionSandboxProfile {
    #[must_use]
    pub fn spec(self) -> ExecutionSandboxProfileSpec {
        match self {
            Self::Unconfined => ExecutionSandboxProfileSpec {
                profile: self,
                summary: "unconfined local execution",
                requires_sandbox: false,
                requires_network_isolation: false,
                network_allowed: true,
                dependency_caches_read_only: false,
            },
            Self::WorkspaceWrite => ExecutionSandboxProfileSpec {
                profile: self,
                summary: "workspace-write sandbox",
                requires_sandbox: true,
                requires_network_isolation: false,
                network_allowed: false,
                dependency_caches_read_only: false,
            },
            Self::BuildOffline => ExecutionSandboxProfileSpec {
                profile: self,
                summary: "offline build sandbox with read-only dependency caches",
                requires_sandbox: true,
                requires_network_isolation: true,
                network_allowed: false,
                dependency_caches_read_only: true,
            },
            Self::BuildNetworked => ExecutionSandboxProfileSpec {
                profile: self,
                summary: "networked build sandbox with read-only dependency caches",
                requires_sandbox: true,
                requires_network_isolation: false,
                network_allowed: true,
                dependency_caches_read_only: true,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ExecutionSandboxProfileSpec {
    pub profile: ExecutionSandboxProfile,
    pub summary: &'static str,
    pub requires_sandbox: bool,
    pub requires_network_isolation: bool,
    pub network_allowed: bool,
    pub dependency_caches_read_only: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionCoverageLabel {
    KernelMediated,
    LocalBackendEnforced,
    ExternalMcpServer,
    PluginManaged,
    RemoteService,
    UnknownExternal,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ExecutionCoverageSummary {
    pub label: ExecutionCoverageLabel,
    pub local_backend_controls_execution: bool,
    pub user_copy: &'static str,
}

impl ExecutionCoverageSummary {
    #[must_use]
    pub fn for_tool_category(category: ToolCategory) -> Self {
        match category {
            ToolCategory::Shell => Self {
                label: ExecutionCoverageLabel::LocalBackendEnforced,
                local_backend_controls_execution: true,
                user_copy: "shell commands use the configured local execution backend",
            },
            ToolCategory::Mcp => Self {
                label: ExecutionCoverageLabel::ExternalMcpServer,
                local_backend_controls_execution: false,
                user_copy: "MCP tools run in their server boundary; local shell sandbox does not cover them",
            },
            ToolCategory::Custom => Self {
                label: ExecutionCoverageLabel::UnknownExternal,
                local_backend_controls_execution: false,
                user_copy: "custom tools must declare their own execution boundary",
            },
            ToolCategory::File | ToolCategory::Search | ToolCategory::Agent => Self {
                label: ExecutionCoverageLabel::KernelMediated,
                local_backend_controls_execution: false,
                user_copy: "kernel-mediated tool; local shell sandbox is not the execution boundary",
            },
        }
    }

    #[must_use]
    pub fn plugin_managed() -> Self {
        Self {
            label: ExecutionCoverageLabel::PluginManaged,
            local_backend_controls_execution: false,
            user_copy: "plugin capability is governed by plugin trust; local shell sandbox does not cover plugin code",
        }
    }

    #[must_use]
    pub fn remote_service() -> Self {
        Self {
            label: ExecutionCoverageLabel::RemoteService,
            local_backend_controls_execution: false,
            user_copy: "remote execution is outside the local shell sandbox",
        }
    }
}

impl ExecutionConfig {
    #[must_use]
    pub fn profile_spec(&self) -> ExecutionSandboxProfileSpec {
        self.profile.spec()
    }

    #[must_use]
    pub fn requires_sandbox(&self) -> bool {
        self.isolation.requires_sandbox() || self.profile_spec().requires_sandbox
    }

    pub fn validate_profile_capabilities(
        &self,
        capabilities: ExecutionBackendCapabilities,
    ) -> std::result::Result<(), String> {
        let spec = self.profile_spec();
        if self.requires_sandbox() && !capabilities.supports_required_sandbox() {
            if self.isolation.requires_sandbox() && !spec.requires_sandbox {
                return Err(
                    "execution isolation require_sandbox requires filesystem and process isolation"
                        .to_owned(),
                );
            }
            return Err(format!(
                "execution profile {:?} requires filesystem and process isolation",
                self.profile
            ));
        }
        if spec.requires_network_isolation && !capabilities.network_isolation {
            return Err(format!(
                "execution profile {:?} requires network isolation",
                self.profile
            ));
        }
        Ok(())
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
