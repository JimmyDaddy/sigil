use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::{
    config::{McpServerConfig, McpServerStartup},
    permission::ApprovalMode,
    session::{ControlEntry, SessionLogEntry},
};

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
        if self.version.trim().is_empty() {
            bail!("plugin {} has empty version", self.id);
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
        let skill_capabilities = self.skills.iter().map(|skill| PluginCapability::Skill {
            path: skill.path.clone(),
        });
        let hook_capabilities = self.hooks.iter().map(|hook| PluginCapability::Hook {
            event: hook.event.clone(),
            command: hook.command.clone(),
            approval: hook.approval,
        });
        let mcp_capabilities = self
            .mcp_servers
            .iter()
            .map(|server| PluginCapability::McpServer {
                name: server.name.clone(),
                command: server.command.clone(),
                startup: server.startup,
                required: server.required,
            });
        skill_capabilities
            .chain(hook_capabilities)
            .chain(mcp_capabilities)
            .collect()
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
    pub event: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub approval: ApprovalMode,
}

impl PluginHookRef {
    /// Validates that the hook is named and has an executable command.
    ///
    /// # Errors
    ///
    /// Returns an error when the event or command is empty.
    pub fn validate(&self) -> Result<()> {
        if self.event.trim().is_empty() {
            bail!("plugin hook has empty event");
        }
        if self.command.trim().is_empty() {
            bail!("plugin hook {} has empty command", self.event);
        }
        Ok(())
    }
}

/// Reviewable capability summary derived from a plugin manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PluginCapability {
    Skill {
        path: PathBuf,
    },
    Hook {
        event: String,
        command: String,
        approval: ApprovalMode,
    },
    McpServer {
        name: String,
        command: String,
        startup: McpServerStartup,
        required: bool,
    },
}

impl PluginCapability {
    /// Validates a capability summary before it is captured in the control log.
    ///
    /// # Errors
    ///
    /// Returns an error when the capability contains unsafe paths or empty command metadata.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Skill { path } => validate_manifest_relative_path("skill", path),
            Self::Hook { event, command, .. } => {
                if event.trim().is_empty() {
                    bail!("plugin hook capability has empty event");
                }
                if command.trim().is_empty() {
                    bail!("plugin hook capability {event} has empty command");
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
        if self.version.trim().is_empty() {
            bail!("plugin {} snapshot has empty version", self.plugin_id);
        }
        if self.manifest_path.as_os_str().is_empty() {
            bail!("plugin {} snapshot has empty manifest path", self.plugin_id);
        }
        if self.manifest_hash.trim().is_empty() {
            bail!("plugin {} snapshot has empty manifest hash", self.plugin_id);
        }
        for capability in &self.capabilities {
            capability.validate()?;
        }
        Ok(())
    }
}

/// Append-only trust review decision for one plugin manifest hash.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PluginTrustEntry {
    pub plugin_id: String,
    pub manifest_path: PathBuf,
    pub manifest_hash: String,
    pub decision: PluginTrustDecision,
    pub reviewed_at_ms: u64,
}

impl PluginTrustEntry {
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
        if self.manifest_hash.trim().is_empty() {
            bail!(
                "plugin {} trust entry has empty manifest hash",
                self.plugin_id
            );
        }
        Ok(())
    }

    pub fn matches_snapshot(&self, snapshot: &PluginManifestSnapshot) -> bool {
        self.plugin_id == snapshot.plugin_id
            && self.manifest_path == snapshot.manifest_path
            && self.manifest_hash == snapshot.manifest_hash
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
            match entry {
                SessionLogEntry::Control(ControlEntry::PluginManifestCaptured(snapshot)) => {
                    projection.apply_manifest(snapshot);
                }
                SessionLogEntry::Control(ControlEntry::PluginTrustDecision(entry)) => {
                    projection.apply_trust(entry);
                }
                SessionLogEntry::User(_)
                | SessionLogEntry::Assistant(_)
                | SessionLogEntry::ToolResult(_)
                | SessionLogEntry::Control(_) => {}
            }
        }
        projection
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
