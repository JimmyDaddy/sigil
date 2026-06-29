use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    config::{McpServerConfig, McpServerStartup},
    execution_backend::ExecutionCoverageSummary,
    permission::ApprovalMode,
    session::{ControlEntry, SessionLogEntry},
    tool::{ToolAccess, ToolCategory},
    verification::ToolEffect,
};

fn default_plugin_egress_logging() -> bool {
    true
}

fn default_plugin_hook_timeout_ms() -> u64 {
    DEFAULT_PLUGIN_HOOK_TIMEOUT_MS
}

fn default_plugin_hook_declared_effect() -> ToolEffect {
    ToolEffect::Unknown
}

/// Canonical prefix for plugin manifest content digests.
pub const PLUGIN_MANIFEST_DIGEST_PREFIX: &str = "sha256:";

/// Default bounded runtime for a plugin hook command declared in static manifest data.
pub const DEFAULT_PLUGIN_HOOK_TIMEOUT_MS: u64 = 30_000;

/// Maximum hook timeout accepted in a plugin manifest.
pub const MAX_PLUGIN_HOOK_TIMEOUT_MS: u64 = 600_000;

/// Provider-neutral manifest for one local capability package.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PluginManifest {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub root: PathBuf,
    #[serde(default)]
    pub agents: Vec<PluginAgentRef>,
    #[serde(default)]
    pub skills: Vec<PluginSkillRef>,
    #[serde(default)]
    pub hooks: Vec<PluginHookRef>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
}

impl PluginManifest {
    /// Validates provider-neutral manifest fields before runtime registration.
    ///
    /// # Errors
    ///
    /// Returns an error when ids, manifest-relative paths, hook commands, or MCP metadata are
    /// structurally unsafe.
    pub fn validate(&self) -> Result<()> {
        validate_plugin_id(&self.id)?;
        if self.name.trim().is_empty() {
            bail!("plugin {} has empty name", self.id);
        }
        validate_plugin_version(&self.id, &self.version)?;
        for agent in &self.agents {
            agent.validate()?;
        }
        for skill in &self.skills {
            skill.validate()?;
        }
        for hook in &self.hooks {
            hook.validate()?;
        }
        for server in &self.mcp_servers {
            validate_plugin_id(&server.name)?;
            if server.command.trim().is_empty() {
                bail!(
                    "plugin {} MCP server {} has empty command",
                    self.id,
                    server.name
                );
            }
        }
        Ok(())
    }

    /// Projects manifest entries into reviewable capability summaries.
    pub fn capabilities(&self) -> Vec<PluginCapability> {
        let agent_capabilities = self.agents.iter().map(|agent| PluginCapability::Agent {
            path: agent.path.clone(),
        });
        let skill_capabilities = self.skills.iter().map(|skill| PluginCapability::Skill {
            path: skill.path.clone(),
        });
        let hook_capabilities = self.hooks.iter().map(|hook| PluginCapability::Hook {
            id: hook.stable_id(),
            event: hook.event.clone(),
            hook_kind: hook.kind,
            command: hook.command.clone(),
            args: hook.args.clone(),
            declared_effect: hook.declared_effect,
            timeout_ms: hook.timeout_ms,
            input_schema_digest: hook.input_schema_digest.clone(),
            output_schema_digest: hook.output_schema_digest.clone(),
            approval: hook.approval,
            egress_logging: hook.egress_logging,
            allow_secrets: hook.allow_secrets,
        });
        let mcp_capabilities = self
            .mcp_servers
            .iter()
            .map(|server| PluginCapability::McpServer {
                name: server.name.clone(),
                command: server.command.clone(),
                args: server.args.clone(),
                startup: server.startup,
                required: server.required,
                approval: server.trust.approval_default,
                egress_logging: server.trust.egress_logging,
                allow_secrets: server.trust.allow_secrets,
            });
        agent_capabilities
            .chain(skill_capabilities)
            .chain(hook_capabilities)
            .chain(mcp_capabilities)
            .collect()
    }
}

/// One agent profile entry declared by a plugin manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PluginAgentRef {
    pub path: PathBuf,
}

impl PluginAgentRef {
    /// Validates that the agent profile path is manifest-relative and cannot escape the plugin root.
    ///
    /// # Errors
    ///
    /// Returns an error when the path is empty, absolute, or contains parent traversal.
    pub fn validate(&self) -> Result<()> {
        validate_manifest_relative_path("agent", &self.path)
    }
}

/// One skill entry declared by a plugin manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PluginSkillRef {
    pub path: PathBuf,
}

impl PluginSkillRef {
    /// Validates that the skill path is manifest-relative and cannot escape the plugin root.
    ///
    /// # Errors
    ///
    /// Returns an error when the path is empty, absolute, or contains parent traversal.
    pub fn validate(&self) -> Result<()> {
        validate_manifest_relative_path("skill", &self.path)
    }
}

/// One hook command declared by a plugin manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PluginHookRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub event: String,
    #[serde(default)]
    pub kind: PluginHookKind,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_plugin_hook_declared_effect")]
    pub declared_effect: ToolEffect,
    #[serde(default = "default_plugin_hook_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema_digest: Option<String>,
    #[serde(default)]
    pub approval: ApprovalMode,
    #[serde(default = "default_plugin_egress_logging")]
    pub egress_logging: bool,
    #[serde(default)]
    pub allow_secrets: bool,
}

impl PluginHookRef {
    /// Returns the stable hook id used in review, digest and future runtime records.
    #[must_use]
    pub fn stable_id(&self) -> String {
        self.id
            .as_ref()
            .filter(|id| !id.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| self.event.clone())
    }

    /// Returns the static command vector declared by the plugin manifest.
    #[must_use]
    pub fn command_vector(&self) -> Vec<String> {
        let mut command = Vec::with_capacity(1 + self.args.len());
        command.push(self.command.clone());
        command.extend(self.args.clone());
        command
    }

    /// Validates that the hook has a stable identity and bounded executable command contract.
    ///
    /// # Errors
    ///
    /// Returns an error when the hook id, event, command vector, timeout or schema digests are
    /// structurally unsafe.
    pub fn validate(&self) -> Result<()> {
        let id = self.stable_id();
        validate_plugin_id(&id)?;
        if self.event.trim().is_empty() {
            bail!("plugin hook {id} has empty event");
        }
        if self.command.trim().is_empty() {
            bail!("plugin hook {id} has empty command");
        }
        validate_plugin_command_segment("hook command", &self.command)?;
        for arg in &self.args {
            validate_plugin_command_segment("hook argument", arg)?;
        }
        if self.timeout_ms == 0 || self.timeout_ms > MAX_PLUGIN_HOOK_TIMEOUT_MS {
            bail!("plugin hook {id} timeout_ms must be between 1 and {MAX_PLUGIN_HOOK_TIMEOUT_MS}");
        }
        if let Some(digest) = &self.input_schema_digest {
            validate_plugin_hook_schema_digest(&id, "input", digest)?;
        }
        if let Some(digest) = &self.output_schema_digest {
            validate_plugin_hook_schema_digest(&id, "output", digest)?;
        }
        Ok(())
    }
}

/// Static hook category declared by plugin manifest data.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginHookKind {
    Context,
    Compaction,
    Verification,
    #[default]
    Event,
}

/// Reviewable capability summary derived from a plugin manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PluginCapability {
    Agent {
        path: PathBuf,
    },
    Skill {
        path: PathBuf,
    },
    Hook {
        #[serde(default)]
        id: String,
        event: String,
        #[serde(default)]
        hook_kind: PluginHookKind,
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default = "default_plugin_hook_declared_effect")]
        declared_effect: ToolEffect,
        #[serde(default = "default_plugin_hook_timeout_ms")]
        timeout_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_schema_digest: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_schema_digest: Option<String>,
        approval: ApprovalMode,
        #[serde(default = "default_plugin_egress_logging")]
        egress_logging: bool,
        #[serde(default)]
        allow_secrets: bool,
    },
    McpServer {
        name: String,
        command: String,
        #[serde(default)]
        args: Vec<String>,
        startup: McpServerStartup,
        required: bool,
        #[serde(default)]
        approval: ApprovalMode,
        #[serde(default = "default_plugin_egress_logging")]
        egress_logging: bool,
        #[serde(default)]
        allow_secrets: bool,
    },
}

impl PluginCapability {
    /// Returns the normal tool/security policy that this capability must integrate with before it
    /// can execute.
    ///
    /// This is review/audit metadata only. It does not grant execution by itself, and future
    /// runtime code must still enforce the corresponding tool permission, execution backend,
    /// egress logging, secret access and mutation recording paths.
    #[must_use]
    pub fn policy_summary(&self) -> PluginCapabilityPolicy {
        match self {
            Self::Agent { .. } | Self::Skill { .. } => PluginCapabilityPolicy {
                tool_category: None,
                tool_access: None,
                approval_default: None,
                execution_backend_required: false,
                egress_logging: false,
                allow_secrets: false,
                mutation_effect: ToolEffect::ReadOnly,
            },
            Self::Hook {
                approval,
                egress_logging,
                allow_secrets,
                declared_effect,
                ..
            } => PluginCapabilityPolicy {
                tool_category: Some(ToolCategory::Custom),
                tool_access: Some(ToolAccess::Execute),
                approval_default: Some(*approval),
                execution_backend_required: true,
                egress_logging: *egress_logging,
                allow_secrets: *allow_secrets,
                mutation_effect: *declared_effect,
            },
            Self::McpServer {
                approval,
                egress_logging,
                allow_secrets,
                ..
            } => PluginCapabilityPolicy {
                tool_category: Some(ToolCategory::Mcp),
                tool_access: Some(ToolAccess::Network),
                approval_default: Some(*approval),
                execution_backend_required: true,
                egress_logging: *egress_logging,
                allow_secrets: *allow_secrets,
                mutation_effect: ToolEffect::Unknown,
            },
        }
    }

    /// Returns the execution-boundary summary for this plugin capability.
    ///
    /// This describes whether Sigil's local execution backend controls the capability execution.
    /// It is intentionally separate from trust approval: a trusted MCP server or plugin hook may
    /// still run outside the local shell sandbox boundary.
    #[must_use]
    pub fn execution_coverage_summary(&self) -> ExecutionCoverageSummary {
        if matches!(self, Self::McpServer { .. }) {
            return ExecutionCoverageSummary::for_tool_category(ToolCategory::Mcp);
        }
        ExecutionCoverageSummary::plugin_managed()
    }

    /// Validates a capability summary before it is captured in the control log.
    ///
    /// # Errors
    ///
    /// Returns an error when the capability contains unsafe paths or empty command metadata.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Agent { path } => validate_manifest_relative_path("agent", path),
            Self::Skill { path } => validate_manifest_relative_path("skill", path),
            Self::Hook {
                id,
                event,
                command,
                args,
                timeout_ms,
                input_schema_digest,
                output_schema_digest,
                ..
            } => {
                let id = if id.trim().is_empty() { event } else { id };
                validate_plugin_id(id)?;
                if event.trim().is_empty() {
                    bail!("plugin hook capability {id} has empty event");
                }
                if command.trim().is_empty() {
                    bail!("plugin hook capability {id} has empty command");
                }
                validate_plugin_command_segment("hook command", command)?;
                for arg in args {
                    validate_plugin_command_segment("hook argument", arg)?;
                }
                if *timeout_ms == 0 || *timeout_ms > MAX_PLUGIN_HOOK_TIMEOUT_MS {
                    bail!(
                        "plugin hook capability {id} timeout_ms must be between 1 and {MAX_PLUGIN_HOOK_TIMEOUT_MS}"
                    );
                }
                if let Some(digest) = input_schema_digest {
                    validate_plugin_hook_schema_digest(id, "input", digest)?;
                }
                if let Some(digest) = output_schema_digest {
                    validate_plugin_hook_schema_digest(id, "output", digest)?;
                }
                Ok(())
            }
            Self::McpServer { name, command, .. } => {
                validate_plugin_id(name)?;
                if command.trim().is_empty() {
                    bail!("plugin MCP capability {name} has empty command");
                }
                Ok(())
            }
        }
    }
}

/// Static review summary showing how one plugin capability maps back to Sigil's normal tool
/// permission, execution, egress, secret and mutation audit path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PluginCapabilityPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_category: Option<ToolCategory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_access: Option<ToolAccess>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_default: Option<ApprovalMode>,
    pub execution_backend_required: bool,
    pub egress_logging: bool,
    pub allow_secrets: bool,
    pub mutation_effect: ToolEffect,
}

/// Trust state for one plugin manifest hash.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginTrustDecision {
    Trusted,
    #[default]
    NeedsReview,
    Disabled,
}

impl PluginTrustDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trusted => "trusted",
            Self::NeedsReview => "needs_review",
            Self::Disabled => "disabled",
        }
    }
}

/// Durable snapshot of one discovered plugin manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PluginManifestSnapshot {
    pub plugin_id: String,
    #[serde(default)]
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub manifest_path: PathBuf,
    pub manifest_hash: String,
    #[serde(default)]
    pub capabilities: Vec<PluginCapability>,
    #[serde(default)]
    pub trust: PluginTrustDecision,
}

impl PluginManifestSnapshot {
    /// Validates captured manifest metadata before it is persisted.
    ///
    /// # Errors
    ///
    /// Returns an error when the snapshot cannot safely identify the manifest or capabilities.
    pub fn validate(&self) -> Result<()> {
        validate_plugin_id(&self.plugin_id)?;
        validate_plugin_version(&self.plugin_id, &self.version)?;
        if self.manifest_path.as_os_str().is_empty() {
            bail!("plugin {} snapshot has empty manifest path", self.plugin_id);
        }
        validate_plugin_manifest_digest(&self.plugin_id, &self.manifest_hash)?;
        for capability in &self.capabilities {
            capability.validate()?;
        }
        Ok(())
    }

    /// Returns a deterministic digest of the reviewable capabilities projected from the manifest.
    ///
    /// # Errors
    ///
    /// Returns an error if capability serialization fails.
    pub fn capability_digest(&self) -> Result<String> {
        plugin_capability_digest(&self.capabilities)
    }
}

/// Append-only trust review decision for one plugin manifest hash.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PluginTrustEntry {
    pub plugin_id: String,
    pub manifest_path: PathBuf,
    pub manifest_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_digest: Option<String>,
    pub decision: PluginTrustDecision,
    pub reviewed_at_ms: u64,
}

impl PluginTrustEntry {
    /// Creates a trust entry bound to the current static manifest review subject.
    ///
    /// # Errors
    ///
    /// Returns an error if the snapshot is invalid or its capability digest cannot be computed.
    pub fn for_snapshot(
        snapshot: &PluginManifestSnapshot,
        decision: PluginTrustDecision,
        reviewed_at_ms: u64,
    ) -> Result<Self> {
        snapshot.validate()?;
        Ok(Self {
            plugin_id: snapshot.plugin_id.clone(),
            manifest_path: snapshot.manifest_path.clone(),
            manifest_hash: snapshot.manifest_hash.clone(),
            manifest_version: Some(snapshot.version.clone()),
            capability_digest: Some(snapshot.capability_digest()?),
            decision,
            reviewed_at_ms,
        })
    }

    /// Validates trust metadata before it is persisted.
    ///
    /// # Errors
    ///
    /// Returns an error when required trust identity fields are missing or malformed.
    pub fn validate(&self) -> Result<()> {
        validate_plugin_id(&self.plugin_id)?;
        if self.manifest_path.as_os_str().is_empty() {
            bail!(
                "plugin {} trust entry has empty manifest path",
                self.plugin_id
            );
        }
        validate_plugin_manifest_digest(&self.plugin_id, &self.manifest_hash)?;
        if let Some(version) = &self.manifest_version {
            validate_plugin_version(&self.plugin_id, version)?;
        }
        if let Some(capability_digest) = &self.capability_digest {
            validate_plugin_capability_digest(&self.plugin_id, capability_digest)?;
        }
        Ok(())
    }

    pub fn matches_snapshot(&self, snapshot: &PluginManifestSnapshot) -> bool {
        if self.plugin_id != snapshot.plugin_id
            || self.manifest_path != snapshot.manifest_path
            || !plugin_manifest_digests_match(&self.manifest_hash, &snapshot.manifest_hash)
        {
            return false;
        }
        if let Some(version) = &self.manifest_version
            && version != &snapshot.version
        {
            return false;
        }
        if let Some(capability_digest) = &self.capability_digest {
            let Ok(snapshot_digest) = snapshot.capability_digest() else {
                return false;
            };
            if !plugin_manifest_digests_match(capability_digest, &snapshot_digest) {
                return false;
            }
        }
        true
    }
}

/// Latest plugin state reconstructed from append-only control entries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginStateProjection {
    pub manifests: BTreeMap<String, PluginManifestSnapshot>,
    pub trust_entries: BTreeMap<String, PluginTrustEntry>,
    pub latest_manifest_plugin_id: Option<String>,
    pub latest_trust_plugin_id: Option<String>,
    pub manifest_replay_order: Vec<String>,
    pub trust_replay_order: Vec<String>,
}

impl PluginStateProjection {
    /// Replays append-only session entries into the latest plugin projection.
    pub fn from_entries(entries: &[SessionLogEntry]) -> Self {
        let mut projection = Self::default();
        for entry in entries {
            if let SessionLogEntry::Control(control) = entry {
                projection.apply_control_entry(control);
            }
        }
        projection
    }

    pub(crate) fn apply_control_entry(&mut self, control: &ControlEntry) {
        match control {
            ControlEntry::PluginManifestCaptured(snapshot) => self.apply_manifest(snapshot),
            ControlEntry::PluginTrustDecision(entry) => self.apply_trust(entry),
            _ => {}
        }
    }

    pub fn latest_manifest(&self) -> Option<&PluginManifestSnapshot> {
        self.latest_manifest_plugin_id
            .as_ref()
            .and_then(|id| self.manifests.get(id))
    }

    pub fn latest_trust(&self) -> Option<&PluginTrustEntry> {
        self.latest_trust_plugin_id
            .as_ref()
            .and_then(|id| self.trust_entries.get(id))
    }

    fn apply_manifest(&mut self, snapshot: &PluginManifestSnapshot) {
        self.latest_manifest_plugin_id = Some(snapshot.plugin_id.clone());
        self.manifest_replay_order.push(snapshot.plugin_id.clone());
        let mut snapshot = snapshot.clone();
        snapshot.trust = PluginTrustDecision::NeedsReview;
        if let Some(entry) = self.trust_entries.get(&snapshot.plugin_id)
            && entry.matches_snapshot(&snapshot)
        {
            snapshot.trust = entry.decision;
        }
        self.manifests.insert(snapshot.plugin_id.clone(), snapshot);
    }

    fn apply_trust(&mut self, entry: &PluginTrustEntry) {
        self.latest_trust_plugin_id = Some(entry.plugin_id.clone());
        self.trust_replay_order.push(entry.plugin_id.clone());
        self.trust_entries
            .insert(entry.plugin_id.clone(), entry.clone());
        if let Some(snapshot) = self.manifests.get_mut(&entry.plugin_id)
            && entry.matches_snapshot(snapshot)
        {
            snapshot.trust = entry.decision;
        }
    }
}

/// Validates one plugin id or plugin-owned stable name.
///
/// # Errors
///
/// Returns an error when the value is empty or contains characters outside `[a-zA-Z0-9._-]`.
pub fn validate_plugin_id(id: &str) -> Result<()> {
    let is_valid = !id.is_empty()
        && id
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '.' | '_' | '-'));
    if !is_valid {
        bail!("invalid plugin id {id:?}");
    }
    Ok(())
}

/// Validates a plugin version string without forcing a package ecosystem-specific semver.
///
/// # Errors
///
/// Returns an error when the version is empty, path-like, contains whitespace/control characters,
/// or is too long for review surfaces.
pub fn validate_plugin_version(plugin_id: &str, version: &str) -> Result<()> {
    if version.trim().is_empty() {
        bail!("plugin {plugin_id} has empty version");
    }
    if version.len() > 128 {
        bail!("plugin {plugin_id} version is too long");
    }
    if version
        .chars()
        .any(|value| value.is_ascii_control() || value.is_ascii_whitespace())
    {
        bail!("plugin {plugin_id} version cannot contain whitespace or control characters");
    }
    if version.contains('/') || version.contains('\\') || version.contains("..") {
        bail!("plugin {plugin_id} version cannot be path-like");
    }
    Ok(())
}

/// Validates a plugin manifest content digest.
///
/// New snapshots use `sha256:<64 lowercase hex>`. Bare 64-character SHA-256 values are accepted
/// only for compatibility with manifests captured before the prefix was introduced.
///
/// # Errors
///
/// Returns an error when the digest is empty, has an unsupported prefix, has the wrong length, or
/// contains non-hex characters.
pub fn validate_plugin_manifest_digest(plugin_id: &str, digest: &str) -> Result<()> {
    normalize_plugin_manifest_digest(digest)
        .map(|_| ())
        .ok_or_else(|| anyhow::anyhow!("plugin {plugin_id} manifest hash must be a SHA-256 digest"))
}

/// Validates a plugin capability digest.
///
/// # Errors
///
/// Returns an error when the digest is not a SHA-256 digest.
pub fn validate_plugin_capability_digest(plugin_id: &str, digest: &str) -> Result<()> {
    normalize_plugin_manifest_digest(digest)
        .map(|_| ())
        .ok_or_else(|| {
            anyhow::anyhow!("plugin {plugin_id} capability digest must be a SHA-256 digest")
        })
}

/// Validates a plugin hook schema digest.
///
/// # Errors
///
/// Returns an error when the digest is not a SHA-256 digest.
pub fn validate_plugin_hook_schema_digest(
    hook_id: &str,
    schema_kind: &str,
    digest: &str,
) -> Result<()> {
    normalize_plugin_manifest_digest(digest)
        .map(|_| ())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "plugin hook {hook_id} {schema_kind} schema digest must be a SHA-256 digest"
            )
        })
}

/// Returns true when two manifest digests identify the same content.
///
/// This accepts prefixed-vs-bare SHA-256 compatibility but does not match malformed digests.
#[must_use]
pub fn plugin_manifest_digests_match(left: &str, right: &str) -> bool {
    let Some(left) = normalize_plugin_manifest_digest(left) else {
        return false;
    };
    let Some(right) = normalize_plugin_manifest_digest(right) else {
        return false;
    };
    left.eq_ignore_ascii_case(right)
}

fn normalize_plugin_manifest_digest(digest: &str) -> Option<&str> {
    if digest.is_empty() || digest.trim() != digest {
        return None;
    }
    let value = digest;
    let value = value
        .strip_prefix(PLUGIN_MANIFEST_DIGEST_PREFIX)
        .unwrap_or(value);
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Some(value)
    } else {
        None
    }
}

fn validate_plugin_command_segment(kind: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        bail!("plugin {kind} cannot be empty");
    }
    if value
        .chars()
        .any(|character| character == '\0' || character.is_ascii_control())
    {
        bail!("plugin {kind} cannot contain control characters");
    }
    Ok(())
}

fn plugin_capability_digest(capabilities: &[PluginCapability]) -> Result<String> {
    let bytes = serde_json::to_vec(capabilities)?;
    Ok(format!(
        "{}{:x}",
        PLUGIN_MANIFEST_DIGEST_PREFIX,
        Sha256::digest(&bytes)
    ))
}

fn validate_manifest_relative_path(kind: &str, path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() {
        bail!("plugin {kind} path is empty");
    }
    if path.is_absolute() {
        bail!("plugin {kind} path must be manifest-relative");
    }
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!(
                    "plugin {kind} path cannot escape plugin root: {}",
                    path.display()
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/plugin_tests.rs"]
mod tests;
