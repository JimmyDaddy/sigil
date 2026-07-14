use std::{collections::BTreeMap, path::PathBuf, time::Duration};

use anyhow::Result;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};

use crate::{
    permission::NetworkPolicy,
    process_environment::{
        ExtensionProcessLaunchError, ExtensionProcessLaunchErrorCode, ProcessEnvironmentPolicy,
    },
    tool::{NetworkEffect, ToolCategory},
};

/// Stable identifier for an execution backend implementation.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionBackendKind {
    #[default]
    Local,
    MacosSeatbelt,
    LinuxBubblewrap,
    Docker,
}

impl ExecutionBackendKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::MacosSeatbelt => "macos_seatbelt",
            Self::LinuxBubblewrap => "linux_bubblewrap",
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

/// Network policy outcome reported by an execution backend for a single command.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionNetworkPolicy {
    /// Network access was intentionally allowed for this execution.
    Allowed,
    /// Network access was denied by a backend with network enforcement.
    Denied,
    /// The backend cannot enforce the requested network policy.
    Unsupported,
    /// No reliable network policy information was available.
    #[default]
    Unknown,
}

impl ExecutionNetworkPolicy {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Denied => "denied",
            Self::Unsupported => "unsupported",
            Self::Unknown => "unknown",
        }
    }
}

/// Auditable network policy receipt attached to an execution receipt.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ExecutionNetworkReceipt {
    pub policy: ExecutionNetworkPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl ExecutionNetworkReceipt {
    #[must_use]
    pub fn allowed(reason: impl Into<String>) -> Self {
        Self {
            policy: ExecutionNetworkPolicy::Allowed,
            reason: Some(reason.into()),
        }
    }

    #[must_use]
    pub fn denied(reason: impl Into<String>) -> Self {
        Self {
            policy: ExecutionNetworkPolicy::Denied,
            reason: Some(reason.into()),
        }
    }

    #[must_use]
    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self {
            policy: ExecutionNetworkPolicy::Unsupported,
            reason: Some(reason.into()),
        }
    }

    #[must_use]
    pub fn unknown(reason: impl Into<String>) -> Self {
        Self {
            policy: ExecutionNetworkPolicy::Unknown,
            reason: Some(reason.into()),
        }
    }

    #[must_use]
    pub fn is_denied(&self) -> bool {
        self.policy == ExecutionNetworkPolicy::Denied
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionResourceLimitKind {
    WallClockTimeout,
    CpuTime,
    Memory,
    ProcessCount,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ExecutionResourceLimitReceipt {
    pub kind: ExecutionResourceLimitKind,
    pub value: String,
}

impl ExecutionResourceLimitReceipt {
    #[must_use]
    pub fn new(kind: ExecutionResourceLimitKind, value: impl Into<String>) -> Self {
        Self {
            kind,
            value: value.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionTimeoutSource {
    #[default]
    None,
    WallClock,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionCleanupStatus {
    #[default]
    NotNeeded,
    Completed,
    Failed,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ExecutionCleanupReceipt {
    pub status: ExecutionCleanupStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl ExecutionCleanupReceipt {
    #[must_use]
    pub fn not_needed() -> Self {
        Self {
            status: ExecutionCleanupStatus::NotNeeded,
            reason: None,
        }
    }

    #[must_use]
    pub fn completed(reason: impl Into<String>) -> Self {
        Self {
            status: ExecutionCleanupStatus::Completed,
            reason: Some(reason.into()),
        }
    }

    #[must_use]
    pub fn failed(reason: impl Into<String>) -> Self {
        Self {
            status: ExecutionCleanupStatus::Failed,
            reason: Some(reason.into()),
        }
    }

    #[must_use]
    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self {
            status: ExecutionCleanupStatus::Unsupported,
            reason: Some(reason.into()),
        }
    }

    #[must_use]
    pub fn unknown(reason: impl Into<String>) -> Self {
        Self {
            status: ExecutionCleanupStatus::Unknown,
            reason: Some(reason.into()),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ExecutionResourceReceipt {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied_limits: Vec<ExecutionResourceLimitReceipt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unsupported_limits: Vec<ExecutionResourceLimitReceipt>,
    #[serde(default)]
    pub timeout_source: ExecutionTimeoutSource,
    #[serde(default)]
    pub cleanup: ExecutionCleanupReceipt,
}

/// Current schema version for bounded process-output evidence embedded in an execution receipt.
pub const EXECUTION_OUTPUT_RECEIPT_SCHEMA_VERSION: u8 = 1;

/// Process output stream that caused a collection or framing terminal condition.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionOutputStream {
    /// Standard output (`stdout`).
    Stdout,
    /// Standard error (`stderr`).
    Stderr,
    /// Aggregate limit or failure shared by both output streams.
    Combined,
}

impl ExecutionOutputStream {
    /// Returns the stable serialized label used by diagnostics and tool-result metadata.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::Combined => "combined",
        }
    }
}

/// Bounded capture statistics for one process output stream.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ExecutionStreamCapture {
    /// Bytes observed from the pipe before it closed or the reader failed.
    pub total_bytes: u64,
    /// Bytes retained in the receipt head/tail buffer.
    pub returned_bytes: u64,
    /// Observed bytes not retained in the receipt.
    pub omitted_bytes: u64,
    /// Prefix bytes retained before any omitted region.
    pub retained_head_bytes: u64,
    /// Suffix bytes retained after any omitted region.
    pub retained_tail_bytes: u64,
    /// Maximum bytes retained in memory for this stream.
    pub retained_limit_bytes: u64,
    /// Maximum observed bytes allowed before forced process-tree cleanup.
    pub hard_limit_bytes: u64,
    /// Lines observed in the original byte stream.
    pub total_lines: u64,
    /// Whether observed bytes were omitted from the receipt.
    pub truncated: bool,
}

/// Terminal reason selected once by the process supervisor.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutionTerminationCause {
    /// The supervised process exited without an output-supervisor terminal condition.
    #[default]
    Exited,
    /// The configured absolute wall-clock deadline elapsed.
    TimedOut,
    /// Cooperative run cancellation selected process-tree cleanup.
    Cancelled,
    /// A per-stream or combined hard output limit was exceeded.
    OutputLimit {
        /// Stream whose limit was exceeded.
        stream: ExecutionOutputStream,
        /// Configured hard ceiling in bytes.
        limit_bytes: u64,
        /// Bytes observed when the supervisor selected this terminal condition.
        observed_bytes: u64,
    },
    /// A pipe reader failed before the stream reached EOF.
    ReaderFailed {
        /// Stream whose reader failed.
        stream: ExecutionOutputStream,
        /// Sanitized diagnostic reason supplied by the collector.
        reason: String,
    },
}

impl ExecutionTerminationCause {
    /// Returns the stable diagnostic label for this terminal cause.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Exited => "exited",
            Self::TimedOut => "timed_out",
            Self::Cancelled => "cancelled",
            Self::OutputLimit { .. } => "output_limit",
            Self::ReaderFailed { .. } => "reader_failed",
        }
    }
}

/// Provider-neutral evidence for bounded stdout/stderr collection.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ExecutionOutputReceipt {
    /// Schema version for this evidence.
    pub schema_version: u8,
    /// Stdout collection evidence.
    pub stdout: ExecutionStreamCapture,
    /// Stderr collection evidence.
    pub stderr: ExecutionStreamCapture,
    /// Total bytes observed across stdout and stderr.
    pub combined_total_bytes: u64,
    /// Combined hard ceiling applied across stdout and stderr.
    pub combined_hard_limit_bytes: u64,
    /// Single terminal cause selected by the process supervisor.
    pub termination: ExecutionTerminationCause,
}

impl Default for ExecutionOutputReceipt {
    fn default() -> Self {
        Self {
            schema_version: EXECUTION_OUTPUT_RECEIPT_SCHEMA_VERSION,
            stdout: ExecutionStreamCapture::default(),
            stderr: ExecutionStreamCapture::default(),
            combined_total_bytes: 0,
            combined_hard_limit_bytes: 0,
            termination: ExecutionTerminationCause::Exited,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
struct ExecutionOutputReceiptWire {
    schema_version: u8,
    stdout: ExecutionStreamCapture,
    stderr: ExecutionStreamCapture,
    combined_total_bytes: u64,
    combined_hard_limit_bytes: u64,
    termination: ExecutionTerminationCause,
}

impl<'de> Deserialize<'de> for ExecutionOutputReceipt {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = ExecutionOutputReceiptWire::deserialize(deserializer)?;
        if wire.schema_version != EXECUTION_OUTPUT_RECEIPT_SCHEMA_VERSION {
            return Err(<D::Error as serde::de::Error>::custom(format!(
                "unsupported execution output receipt schema version {}",
                wire.schema_version
            )));
        }
        Ok(Self {
            schema_version: wire.schema_version,
            stdout: wire.stdout,
            stderr: wire.stderr,
            combined_total_bytes: wire.combined_total_bytes,
            combined_hard_limit_bytes: wire.combined_hard_limit_bytes,
            termination: wire.termination,
        })
    }
}

/// User-configurable execution policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionConfig {
    pub strategy: ExecutionStrategyConfig,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            strategy: ExecutionStrategyConfig::Local,
        }
    }
}

impl Serialize for ExecutionConfig {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        ExecutionConfigWire::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ExecutionConfig {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = ExecutionConfigWire::deserialize(deserializer)?;
        Self::try_from(wire).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct ExecutionConfigWire {
    #[serde(default)]
    strategy: ExecutionStrategyMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sandbox: Option<ExecutionSandboxStrategyConfig>,
}

impl From<&ExecutionConfig> for ExecutionConfigWire {
    fn from(config: &ExecutionConfig) -> Self {
        match &config.strategy {
            ExecutionStrategyConfig::Local => Self {
                strategy: ExecutionStrategyMode::Local,
                sandbox: None,
            },
            ExecutionStrategyConfig::Sandbox(sandbox) => Self {
                strategy: ExecutionStrategyMode::Sandbox,
                sandbox: Some(sandbox.clone()),
            },
        }
    }
}

impl TryFrom<ExecutionConfigWire> for ExecutionConfig {
    type Error = String;

    fn try_from(wire: ExecutionConfigWire) -> std::result::Result<Self, Self::Error> {
        match (wire.strategy, wire.sandbox) {
            (ExecutionStrategyMode::Local, None) => Ok(Self::default()),
            (ExecutionStrategyMode::Local, Some(_)) => Err(
                "execution.sandbox is only valid when execution.strategy is \"sandbox\"".to_owned(),
            ),
            (ExecutionStrategyMode::Sandbox, None) => Err(
                "execution.strategy \"sandbox\" requires an [execution.sandbox] table".to_owned(),
            ),
            (ExecutionStrategyMode::Sandbox, Some(sandbox)) => {
                sandbox.validate()?;
                Ok(Self {
                    strategy: ExecutionStrategyConfig::Sandbox(sandbox),
                })
            }
        }
    }
}

/// Top-level execution strategy. Local preserves ordinary process behavior; sandbox requires an
/// explicit sandbox backend config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionStrategyConfig {
    Local,
    Sandbox(ExecutionSandboxStrategyConfig),
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStrategyMode {
    #[default]
    Local,
    Sandbox,
}

impl ExecutionStrategyMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Sandbox => "sandbox",
        }
    }
}

/// Advanced sandbox backend configuration used only when `execution.strategy = "sandbox"`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ExecutionSandboxStrategyConfig {
    pub backend: ExecutionBackendKind,
    #[serde(default = "default_execution_sandbox_profile")]
    pub profile: ExecutionSandboxProfile,
    #[serde(default)]
    pub fallback: ExecutionSandboxFallback,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_image: Option<String>,
}

impl ExecutionSandboxStrategyConfig {
    #[must_use]
    pub fn new(backend: ExecutionBackendKind) -> Self {
        Self {
            backend,
            profile: default_execution_sandbox_profile(),
            fallback: ExecutionSandboxFallback::default(),
            container_image: None,
        }
    }

    fn validate(&self) -> std::result::Result<(), String> {
        if self.backend == ExecutionBackendKind::Local {
            return Err(
                "execution.strategy \"sandbox\" cannot use execution.sandbox.backend \"local\""
                    .to_owned(),
            );
        }
        if self.profile == ExecutionSandboxProfile::Unconfined {
            return Err(
                "execution.strategy \"sandbox\" cannot use execution.sandbox.profile \"unconfined\""
                    .to_owned(),
            );
        }
        let has_image = self
            .container_image
            .as_deref()
            .is_some_and(|image| !image.trim().is_empty());
        if self.backend == ExecutionBackendKind::Docker && !has_image {
            return Err(
                "execution.sandbox.backend \"docker\" requires execution.sandbox.container_image"
                    .to_owned(),
            );
        }
        if self.backend != ExecutionBackendKind::Docker && self.container_image.is_some() {
            return Err(
                "execution.sandbox.container_image is only valid for docker execution backend"
                    .to_owned(),
            );
        }
        Ok(())
    }
}

fn default_execution_sandbox_profile() -> ExecutionSandboxProfile {
    ExecutionSandboxProfile::WorkspaceWrite
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
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unconfined => "unconfined",
            Self::WorkspaceWrite => "workspace_write",
            Self::BuildOffline => "build_offline",
            Self::BuildNetworked => "build_networked",
        }
    }

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

/// Ephemeral network authorization carried to an extension process pre-spawn boundary.
///
/// This value is intentionally neither configuration nor durable state. The effective policy is
/// resolved for one execution, while `explicit_approval` is supplied only after the agent observes
/// a user approval for an `ask` network facet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExtensionProcessNetworkAdmission {
    pub policy: NetworkPolicy,
    pub explicit_approval: bool,
}

impl ExtensionProcessNetworkAdmission {
    #[must_use]
    pub const fn new(policy: NetworkPolicy, explicit_approval: bool) -> Self {
        Self {
            policy,
            explicit_approval,
        }
    }
}

/// Validates extension process-tree isolation before a backend may spawn a process.
///
/// This uses the existing execution profile as the launch intent. Both static capability and the
/// concrete backend instance's planned network receipt are required so a backend that can isolate
/// networking but is configured to allow it cannot pass a deny preflight.
///
/// # Errors
///
/// Returns a typed pre-spawn error when required process-tree isolation is unavailable, or when a
/// network-deny profile is not bound to a denied network launch plan.
pub fn validate_extension_process_isolation(
    profile: ExecutionSandboxProfile,
    capabilities: ExecutionBackendCapabilities,
    planned_network: &ExecutionNetworkReceipt,
    subject: impl Into<String>,
) -> std::result::Result<(), ExtensionProcessLaunchError> {
    let subject = subject.into();
    let spec = profile.spec();
    if spec.requires_sandbox && !capabilities.process_isolation {
        return Err(ExtensionProcessLaunchError::isolation_unavailable(
            ExtensionProcessLaunchErrorCode::ProcessIsolationUnavailable,
            subject,
            format!(
                "extension process profile {} requires process-tree isolation before spawn",
                profile.as_str()
            ),
        ));
    }
    if !spec.network_allowed
        && (!capabilities.network_isolation
            || !capabilities.process_isolation
            || !planned_network.is_denied())
    {
        return Err(ExtensionProcessLaunchError::isolation_unavailable(
            ExtensionProcessLaunchErrorCode::NetworkIsolationUnavailable,
            subject,
            format!(
                "extension process profile {} denies network access but the backend launch plan is {} and cannot prove network and process-tree isolation",
                profile.as_str(),
                planned_network.policy.as_str()
            ),
        ));
    }
    Ok(())
}

/// Applies the independent network policy to an extension process launch plan.
///
/// A denied network policy may still launch a declared network-capable process only when the
/// backend proves both process-tree and network isolation and plans a denied network receipt.
///
/// # Errors
///
/// Returns a typed pre-spawn error when either the execution profile or independent network
/// policy cannot be enforced by the selected backend plan.
pub fn validate_extension_process_isolation_with_network_policy(
    profile: ExecutionSandboxProfile,
    network_effect: Option<NetworkEffect>,
    network_policy: NetworkPolicy,
    capabilities: ExecutionBackendCapabilities,
    planned_network: &ExecutionNetworkReceipt,
    subject: impl Into<String>,
) -> std::result::Result<(), ExtensionProcessLaunchError> {
    validate_extension_process_network_admission(
        profile,
        network_effect,
        ExtensionProcessNetworkAdmission::new(network_policy, false),
        capabilities,
        planned_network,
        subject,
    )
}

/// Applies one resolved network admission to an extension process launch plan.
///
/// A declared network effect under `ask` requires an explicit user approval carrier. A denied
/// network policy may still launch only when the backend proves both process-tree and network
/// isolation and plans a denied network receipt. Tools without a declared network effect add no
/// independent network gate, but the selected execution profile is always validated.
///
/// # Errors
///
/// Returns [`ExtensionProcessLaunchErrorCode::NetworkApprovalRequired`] when an `ask` admission
/// lacks explicit user approval, or a typed isolation error when the execution profile or denied
/// network policy cannot be enforced by the selected backend plan.
pub fn validate_extension_process_network_admission(
    profile: ExecutionSandboxProfile,
    network_effect: Option<NetworkEffect>,
    admission: ExtensionProcessNetworkAdmission,
    capabilities: ExecutionBackendCapabilities,
    planned_network: &ExecutionNetworkReceipt,
    subject: impl Into<String>,
) -> std::result::Result<(), ExtensionProcessLaunchError> {
    let subject = subject.into();
    if let Some(network_effect) = network_effect
        && admission.policy == NetworkPolicy::Ask
        && !admission.explicit_approval
    {
        return Err(ExtensionProcessLaunchError::network_approval_required(
            subject,
            format!(
                "extension {} network effect requires explicit user approval before spawn",
                network_effect.as_str()
            ),
        ));
    }
    validate_extension_process_isolation(profile, capabilities, planned_network, subject.clone())?;
    if network_effect.is_some()
        && admission.policy == NetworkPolicy::Deny
        && (!capabilities.network_isolation
            || !capabilities.process_isolation
            || !planned_network.is_denied())
    {
        return Err(ExtensionProcessLaunchError::isolation_unavailable(
            ExtensionProcessLaunchErrorCode::NetworkIsolationUnavailable,
            subject,
            format!(
                "extension network policy denies {} effect but the backend launch plan is {} and cannot prove network and process-tree isolation",
                network_effect.map_or("none", NetworkEffect::as_str),
                planned_network.policy.as_str()
            ),
        ));
    }
    Ok(())
}

/// Cross-checks the backend receipt after an extension process ran under network-deny intent.
///
/// # Errors
///
/// Returns a typed receipt error when a network-deny profile did not produce a denied receipt.
pub fn validate_extension_process_network_receipt(
    profile: ExecutionSandboxProfile,
    receipt: &ExecutionNetworkReceipt,
    subject: impl Into<String>,
) -> std::result::Result<(), ExtensionProcessLaunchError> {
    if !profile.spec().network_allowed && !receipt.is_denied() {
        return Err(ExtensionProcessLaunchError::isolation_unavailable(
            ExtensionProcessLaunchErrorCode::BackendReceiptInvalid,
            subject,
            format!(
                "extension process profile {} requires a denied network receipt, observed {}",
                profile.as_str(),
                receipt.policy.as_str()
            ),
        ));
    }
    Ok(())
}

/// Cross-checks a completed extension process receipt against independent network policy.
///
/// # Errors
///
/// Returns a typed receipt error when a declared network effect ran under deny policy without a
/// denied backend receipt.
pub fn validate_extension_process_network_receipt_with_policy(
    profile: ExecutionSandboxProfile,
    network_effect: Option<NetworkEffect>,
    network_policy: NetworkPolicy,
    receipt: &ExecutionNetworkReceipt,
    subject: impl Into<String>,
) -> std::result::Result<(), ExtensionProcessLaunchError> {
    let subject = subject.into();
    validate_extension_process_network_receipt(profile, receipt, subject.clone())?;
    if network_effect.is_some() && network_policy == NetworkPolicy::Deny && !receipt.is_denied() {
        return Err(ExtensionProcessLaunchError::isolation_unavailable(
            ExtensionProcessLaunchErrorCode::BackendReceiptInvalid,
            subject,
            format!(
                "extension network policy denies {:?} effect, observed {} receipt",
                network_effect,
                receipt.policy.as_str()
            ),
        ));
    }
    Ok(())
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
            requested_backend: config.backend(),
            selected_backend: Some(config.backend()),
            requested_profile: config.profile(),
            fallback: config.fallback(),
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
            requested_backend: config.backend(),
            selected_backend: None,
            requested_profile: config.profile(),
            fallback: config.fallback(),
            requirements: config.required_capabilities(),
            capabilities: None,
            missing_capabilities: Vec::new(),
            platform_available: false,
            availability_reason: Some(availability_reason.into()),
            decision: match config.fallback() {
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
            requested_backend: config.backend(),
            selected_backend: None,
            requested_profile: config.profile(),
            fallback: config.fallback(),
            requirements: config.required_capabilities(),
            capabilities: Some(capabilities),
            missing_capabilities: capabilities.missing_requirements(config.required_capabilities()),
            platform_available: true,
            availability_reason: None,
            decision: match config.fallback() {
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

impl ExecutionCoverageLabel {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::KernelMediated => "kernel_mediated",
            Self::LocalBackendEnforced => "local_backend_enforced",
            Self::ExternalMcpServer => "external_mcp_server",
            Self::PluginManaged => "plugin_managed",
            Self::RemoteService => "remote_service",
            Self::UnknownExternal => "unknown_external",
        }
    }
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
    pub fn local() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn sandbox(config: ExecutionSandboxStrategyConfig) -> Self {
        Self {
            strategy: ExecutionStrategyConfig::Sandbox(config),
        }
    }

    #[must_use]
    pub fn strategy_mode(&self) -> ExecutionStrategyMode {
        match self.strategy {
            ExecutionStrategyConfig::Local => ExecutionStrategyMode::Local,
            ExecutionStrategyConfig::Sandbox(_) => ExecutionStrategyMode::Sandbox,
        }
    }

    #[must_use]
    pub fn backend(&self) -> ExecutionBackendKind {
        match &self.strategy {
            ExecutionStrategyConfig::Local => ExecutionBackendKind::Local,
            ExecutionStrategyConfig::Sandbox(config) => config.backend,
        }
    }

    #[must_use]
    pub fn isolation(&self) -> ExecutionIsolationPolicy {
        match self.strategy {
            ExecutionStrategyConfig::Local => ExecutionIsolationPolicy::AllowLocal,
            ExecutionStrategyConfig::Sandbox(_) => ExecutionIsolationPolicy::RequireSandbox,
        }
    }

    #[must_use]
    pub fn profile(&self) -> ExecutionSandboxProfile {
        match &self.strategy {
            ExecutionStrategyConfig::Local => ExecutionSandboxProfile::Unconfined,
            ExecutionStrategyConfig::Sandbox(config) => config.profile,
        }
    }

    #[must_use]
    pub fn fallback(&self) -> ExecutionSandboxFallback {
        match &self.strategy {
            ExecutionStrategyConfig::Local => ExecutionSandboxFallback::Deny,
            ExecutionStrategyConfig::Sandbox(config) => config.fallback,
        }
    }

    #[must_use]
    pub fn container_image(&self) -> Option<&str> {
        match &self.strategy {
            ExecutionStrategyConfig::Local => None,
            ExecutionStrategyConfig::Sandbox(config) => config
                .container_image
                .as_deref()
                .map(str::trim)
                .filter(|image| !image.is_empty()),
        }
    }

    #[must_use]
    pub fn profile_spec(&self) -> ExecutionSandboxProfileSpec {
        self.profile().spec()
    }

    #[must_use]
    pub fn required_capabilities(&self) -> ExecutionCapabilityRequirements {
        let mut requirements = self.profile_spec().requirements;
        if self.isolation().requires_sandbox() {
            requirements.filesystem_isolation = true;
            requirements.process_isolation = true;
        }
        requirements
    }

    #[must_use]
    pub fn required_capabilities_for_persistent_pty(&self) -> ExecutionCapabilityRequirements {
        let mut requirements = self.required_capabilities();
        requirements.persistent_pty = true;
        requirements
    }

    #[must_use]
    pub fn requires_sandbox(&self) -> bool {
        self.isolation().requires_sandbox() || self.required_capabilities().requires_basic_sandbox()
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
            if self.isolation().requires_sandbox() && !spec.requires_sandbox {
                return Err(
                    "execution isolation require_sandbox requires filesystem and process isolation"
                        .to_owned(),
                );
            }
            return Err(format!(
                "execution profile {:?} requires filesystem and process isolation",
                self.profile()
            ));
        }
        if spec.requires_network_isolation
            && missing.contains(&ExecutionCapability::NetworkIsolation)
        {
            return Err(format!(
                "execution profile {:?} requires network isolation",
                self.profile()
            ));
        }
        let missing = missing
            .iter()
            .map(|capability| capability.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        Err(format!(
            "execution profile {:?} requires missing capabilities: {missing}",
            self.profile()
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
    #[serde(default)]
    pub environment_policy: ProcessEnvironmentPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    pub timeout_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_time_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_limit_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_count_limit: Option<u32>,
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

    #[must_use]
    pub fn timeout_millis(&self) -> Option<u64> {
        self.timeout_ms
            .or_else(|| (self.timeout_secs > 0).then(|| self.timeout_secs.saturating_mul(1000)))
    }
}

/// Result captured by an execution backend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ExecutionReceipt {
    pub backend: ExecutionBackendKind,
    pub capabilities: ExecutionBackendCapabilities,
    pub network: ExecutionNetworkReceipt,
    pub resources: ExecutionResourceReceipt,
    pub environment_policy: ProcessEnvironmentPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// Bounded collection statistics and the single supervisor-selected terminal cause.
    pub output: ExecutionOutputReceipt,
    pub timed_out: bool,
}

impl ExecutionReceipt {
    /// Returns the captured structured output evidence.
    #[must_use]
    pub fn effective_output(&self) -> ExecutionOutputReceipt {
        self.output.clone()
    }
}

/// Execution backend for non-interactive commands.
pub type ExecutionFuture<'a> = BoxFuture<'a, Result<ExecutionReceipt>>;

pub trait ExecutionBackend: Send + Sync {
    fn kind(&self) -> ExecutionBackendKind;

    fn capabilities(&self) -> ExecutionBackendCapabilities;

    /// Returns the network policy that this concrete backend instance will apply to its next
    /// execution. Extension-process callers use this pre-spawn plan in addition to capability
    /// flags; capability alone only means that a backend can enforce isolation, not that the
    /// current instance is configured to do so.
    fn planned_network_receipt(&self) -> ExecutionNetworkReceipt {
        ExecutionNetworkReceipt::unknown(
            "execution backend did not declare a pre-spawn network enforcement plan",
        )
    }

    /// Executes one non-interactive command.
    ///
    /// # Errors
    ///
    /// Returns an error when process spawning or supervisor setup fails. Timeouts, output limits,
    /// and pipe reader failures after spawn are represented as successful receipts with structured
    /// output termination evidence, so callers can map them into tool errors without losing
    /// backend and cleanup metadata.
    fn execute(&self, request: ExecutionRequest) -> ExecutionFuture<'_>;

    /// Executes with an optional cooperative cancellation signal.
    fn execute_with_cancellation(
        &self,
        request: ExecutionRequest,
        _cancellation: Option<crate::RunCancellationHandle>,
    ) -> ExecutionFuture<'_> {
        self.execute(request)
    }
}

#[cfg(test)]
#[path = "tests/execution_backend_tests.rs"]
mod tests;
