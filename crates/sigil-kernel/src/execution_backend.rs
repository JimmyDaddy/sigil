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
    Docker,
}

impl ExecutionBackendKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::MacosSeatbelt => "macos_seatbelt",
            Self::Docker => "docker",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionCapability {
    FilesystemIsolation,
    NetworkIsolation,
    ProcessIsolation,
    ResourceLimits,
    PersistentPty,
    WorkspaceSnapshot,
}

impl ExecutionCapability {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FilesystemIsolation => "filesystem_isolation",
            Self::NetworkIsolation => "network_isolation",
            Self::ProcessIsolation => "process_isolation",
            Self::ResourceLimits => "resource_limits",
            Self::PersistentPty => "persistent_pty",
            Self::WorkspaceSnapshot => "workspace_snapshot",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ExecutionCapabilityRequirements {
    pub filesystem_isolation: bool,
    pub network_isolation: bool,
    pub process_isolation: bool,
    pub resource_limits: bool,
    pub persistent_pty: bool,
    pub workspace_snapshot: bool,
}

impl ExecutionCapabilityRequirements {
    #[must_use]
    pub fn requires_basic_sandbox(self) -> bool {
        self.filesystem_isolation && self.process_isolation
    }

    #[must_use]
    pub fn is_empty(self) -> bool {
        !self.filesystem_isolation
            && !self.network_isolation
            && !self.process_isolation
            && !self.resource_limits
            && !self.persistent_pty
            && !self.workspace_snapshot
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

    #[must_use]
    pub fn missing_requirements(
        self,
        requirements: ExecutionCapabilityRequirements,
    ) -> Vec<ExecutionCapability> {
        let mut missing = Vec::new();
        if requirements.filesystem_isolation && !self.filesystem_isolation {
            missing.push(ExecutionCapability::FilesystemIsolation);
        }
        if requirements.network_isolation && !self.network_isolation {
            missing.push(ExecutionCapability::NetworkIsolation);
        }
        if requirements.process_isolation && !self.process_isolation {
            missing.push(ExecutionCapability::ProcessIsolation);
        }
        if requirements.resource_limits && !self.resource_limits {
            missing.push(ExecutionCapability::ResourceLimits);
        }
        if requirements.persistent_pty && !self.persistent_pty {
            missing.push(ExecutionCapability::PersistentPty);
        }
        if requirements.workspace_snapshot && !self.workspace_snapshot {
            missing.push(ExecutionCapability::WorkspaceSnapshot);
        }
        missing
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
    #[serde(default)]
    pub fallback: ExecutionSandboxFallback,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_image: Option<String>,
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

/// What to do when the requested backend cannot satisfy the requested profile.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionSandboxFallback {
    /// Fail closed. This is the only fallback that preserves sandbox invariants without UI input.
    #[default]
    Deny,
    /// Ask the user before relaxing enforcement. Non-interactive entrypoints should treat this as deny.
    Prompt,
    /// Explicitly relax to unconfined local execution. This is an advanced escape hatch.
    Unconfined,
}

impl ExecutionSandboxFallback {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::Prompt => "prompt",
            Self::Unconfined => "unconfined",
        }
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
                requirements: ExecutionCapabilityRequirements::default(),
                requires_network_isolation: false,
                network_allowed: true,
                dependency_caches_read_only: false,
            },
            Self::WorkspaceWrite => ExecutionSandboxProfileSpec {
                profile: self,
                summary: "workspace-write sandbox",
                requires_sandbox: true,
                requirements: ExecutionCapabilityRequirements {
                    filesystem_isolation: true,
                    process_isolation: true,
                    ..ExecutionCapabilityRequirements::default()
                },
                requires_network_isolation: false,
                network_allowed: false,
                dependency_caches_read_only: false,
            },
            Self::BuildOffline => ExecutionSandboxProfileSpec {
                profile: self,
                summary: "offline build sandbox with read-only dependency caches",
                requires_sandbox: true,
                requirements: ExecutionCapabilityRequirements {
                    filesystem_isolation: true,
                    network_isolation: true,
                    process_isolation: true,
                    ..ExecutionCapabilityRequirements::default()
                },
                requires_network_isolation: true,
                network_allowed: false,
                dependency_caches_read_only: true,
            },
            Self::BuildNetworked => ExecutionSandboxProfileSpec {
                profile: self,
                summary: "networked build sandbox with read-only dependency caches",
                requires_sandbox: true,
                requirements: ExecutionCapabilityRequirements {
                    filesystem_isolation: true,
                    process_isolation: true,
                    ..ExecutionCapabilityRequirements::default()
                },
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
    pub requirements: ExecutionCapabilityRequirements,
    pub requires_network_isolation: bool,
    pub network_allowed: bool,
    pub dependency_caches_read_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionBackendSelectionDecision {
    Selected,
    Unavailable,
    MissingCapabilities,
    FallbackDenied,
    FallbackPromptRequired,
    FallbackUnconfined,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ExecutionBackendSelectionDiagnostic {
    pub requested_backend: ExecutionBackendKind,
    pub selected_backend: Option<ExecutionBackendKind>,
    pub requested_profile: ExecutionSandboxProfile,
    pub fallback: ExecutionSandboxFallback,
    pub requirements: ExecutionCapabilityRequirements,
    pub capabilities: Option<ExecutionBackendCapabilities>,
    pub missing_capabilities: Vec<ExecutionCapability>,
    pub platform_available: bool,
    pub availability_reason: Option<String>,
    pub decision: ExecutionBackendSelectionDecision,
}

impl ExecutionBackendSelectionDiagnostic {
    #[must_use]
    pub fn selected(config: &ExecutionConfig, capabilities: ExecutionBackendCapabilities) -> Self {
        Self {
            requested_backend: config.backend,
            selected_backend: Some(config.backend),
            requested_profile: config.profile,
            fallback: config.fallback,
            requirements: config.required_capabilities(),
            capabilities: Some(capabilities),
            missing_capabilities: Vec::new(),
            platform_available: true,
            availability_reason: None,
            decision: ExecutionBackendSelectionDecision::Selected,
        }
    }

    #[must_use]
    pub fn unavailable(config: &ExecutionConfig, availability_reason: impl Into<String>) -> Self {
        Self {
            requested_backend: config.backend,
            selected_backend: None,
            requested_profile: config.profile,
            fallback: config.fallback,
            requirements: config.required_capabilities(),
            capabilities: None,
            missing_capabilities: Vec::new(),
            platform_available: false,
            availability_reason: Some(availability_reason.into()),
            decision: match config.fallback {
                ExecutionSandboxFallback::Deny => ExecutionBackendSelectionDecision::FallbackDenied,
                ExecutionSandboxFallback::Prompt => {
                    ExecutionBackendSelectionDecision::FallbackPromptRequired
                }
                ExecutionSandboxFallback::Unconfined => {
                    ExecutionBackendSelectionDecision::FallbackUnconfined
                }
            },
        }
    }

    #[must_use]
    pub fn missing_capabilities(
        config: &ExecutionConfig,
        capabilities: ExecutionBackendCapabilities,
    ) -> Self {
        Self {
            requested_backend: config.backend,
            selected_backend: None,
            requested_profile: config.profile,
            fallback: config.fallback,
            requirements: config.required_capabilities(),
            capabilities: Some(capabilities),
            missing_capabilities: capabilities.missing_requirements(config.required_capabilities()),
            platform_available: true,
            availability_reason: None,
            decision: match config.fallback {
                ExecutionSandboxFallback::Deny => ExecutionBackendSelectionDecision::FallbackDenied,
                ExecutionSandboxFallback::Prompt => {
                    ExecutionBackendSelectionDecision::FallbackPromptRequired
                }
                ExecutionSandboxFallback::Unconfined => {
                    ExecutionBackendSelectionDecision::FallbackUnconfined
                }
            },
        }
    }

    #[must_use]
    pub fn missing_capability_labels(&self) -> Vec<&'static str> {
        self.missing_capabilities
            .iter()
            .map(|capability| capability.as_str())
            .collect()
    }
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
    pub fn required_capabilities(&self) -> ExecutionCapabilityRequirements {
        let mut requirements = self.profile_spec().requirements;
        if self.isolation.requires_sandbox() {
            requirements.filesystem_isolation = true;
            requirements.process_isolation = true;
        }
        requirements
    }

    #[must_use]
    pub fn requires_sandbox(&self) -> bool {
        self.isolation.requires_sandbox() || self.required_capabilities().requires_basic_sandbox()
    }

    pub fn validate_profile_capabilities(
        &self,
        capabilities: ExecutionBackendCapabilities,
    ) -> std::result::Result<(), String> {
        let spec = self.profile_spec();
        let requirements = self.required_capabilities();
        let missing = capabilities.missing_requirements(requirements);
        if missing.is_empty() {
            return Ok(());
        }
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
        if spec.requires_network_isolation
            && missing.contains(&ExecutionCapability::NetworkIsolation)
        {
            return Err(format!(
                "execution profile {:?} requires network isolation",
                self.profile
            ));
        }
        let missing = missing
            .iter()
            .map(|capability| capability.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        Err(format!(
            "execution profile {:?} requires missing capabilities: {missing}",
            self.profile
        ))
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
