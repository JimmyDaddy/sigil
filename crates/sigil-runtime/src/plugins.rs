use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    McpServerConfig, PluginHookRef, PluginManifest, PluginManifestSnapshot, PluginTrustDecision,
    PluginTrustEntry, SkillDescriptor, SkillIndexSnapshot, validate_plugin_id,
};

use crate::skills::discover_plugin_skill_descriptors;

/// Result of workspace plugin discovery, including review snapshots and trusted registrations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDiscoveryReport {
    pub manifests: Vec<PluginManifestSnapshot>,
    pub registrations: PluginRegistrations,
    pub warnings: Vec<PluginDiscoveryWarning>,
}

/// Runtime registrations emitted by trusted plugin manifests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginRegistrations {
    pub skills: Vec<SkillDescriptor>,
    pub hooks: Vec<PluginHookRegistration>,
    pub mcp_servers: Vec<PluginMcpServerRegistration>,
}

/// Hook registration with explicit plugin source attribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginHookRegistration {
    pub plugin_id: String,
    pub hook: PluginHookRef,
}

/// MCP registration with explicit plugin source attribution and a lifecycle-safe server config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginMcpServerRegistration {
    pub plugin_id: String,
    pub original_name: String,
    pub server: McpServerConfig,
}

/// One non-fatal problem found while discovering workspace plugins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDiscoveryWarning {
    pub kind: PluginDiscoveryWarningKind,
    pub path: PathBuf,
    pub message: String,
}

/// Stable warning categories for plugin diagnostics and future TUI review display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginDiscoveryWarningKind {
    InvalidPath,
    InvalidManifest,
    ReadFailed,
}

impl PluginRegistrations {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty() && self.hooks.is_empty() && self.mcp_servers.is_empty()
    }

    #[must_use]
    pub fn mcp_server_configs(&self) -> Vec<McpServerConfig> {
        self.mcp_servers
            .iter()
            .map(|registration| registration.server.clone())
            .collect()
    }
}

/// Discovers workspace plugins from `.sigil/plugins/<id>/plugin.toml`.
///
/// Untrusted or stale-trust manifests are still returned as review snapshots, but they do not emit
/// skill, hook, or MCP registrations.
///
/// # Errors
///
/// Returns an error if the workspace plugin directory cannot be listed.
pub fn discover_workspace_plugins(
    workspace_root: &Path,
    trust_entries: &[PluginTrustEntry],
) -> Result<PluginDiscoveryReport> {
    let mut discovery = PluginDiscovery::new(workspace_root);
    discovery.discover(trust_entries)?;
    Ok(discovery.finish())
}

/// Merges plugin skill descriptors into an existing deterministic skill snapshot.
///
/// # Errors
///
/// Returns an error when a plugin skill id duplicates an existing descriptor or fingerprinting
/// fails.
pub fn merge_plugin_skill_descriptors(
    snapshot: &SkillIndexSnapshot,
    plugin_skills: &[SkillDescriptor],
) -> Result<SkillIndexSnapshot> {
    let mut seen = snapshot
        .descriptors
        .iter()
        .map(|descriptor| descriptor.id.clone())
        .collect::<BTreeSet<_>>();
    let mut descriptors = snapshot.descriptors.clone();
    for skill in plugin_skills {
        if !seen.insert(skill.id.clone()) {
            bail!(
                "plugin skill {} conflicts with existing skill index",
                skill.id
            );
        }
        descriptors.push(skill.clone());
    }
    SkillIndexSnapshot::new(descriptors)
}

/// Appends plugin-provided MCP server configs to an existing MCP registry input.
///
/// Plugin server names are already namespaced during discovery, so the returned configs can be
/// handed to the existing MCP eager/lazy lifecycle without starting plugin servers early.
#[must_use]
pub fn merge_plugin_mcp_servers(
    base: &[McpServerConfig],
    plugin_servers: &[PluginMcpServerRegistration],
) -> Vec<McpServerConfig> {
    let mut used_names = base
        .iter()
        .map(|server| server.name.clone())
        .collect::<BTreeSet<_>>();
    let mut merged = base.to_vec();
    for registration in plugin_servers {
        let mut server = registration.server.clone();
        if !used_names.insert(server.name.clone()) {
            server.name = unique_plugin_mcp_server_name(
                &registration.plugin_id,
                &registration.original_name,
                &used_names,
            );
            used_names.insert(server.name.clone());
        }
        merged.push(server);
    }
    merged
}

struct PluginDiscovery {
    workspace_root: PathBuf,
    canonical_workspace_root: PathBuf,
    manifests: Vec<PluginManifestSnapshot>,
    registrations: PluginRegistrations,
    warnings: Vec<PluginDiscoveryWarning>,
}

impl PluginDiscovery {
    fn new(workspace_root: &Path) -> Self {
        let canonical_workspace_root = workspace_root
            .canonicalize()
            .unwrap_or_else(|_| workspace_root.to_path_buf());
        Self {
            workspace_root: workspace_root.to_path_buf(),
            canonical_workspace_root,
            manifests: Vec::new(),
            registrations: PluginRegistrations::default(),
            warnings: Vec::new(),
        }
    }

    fn discover(&mut self, trust_entries: &[PluginTrustEntry]) -> Result<()> {
        let plugin_dir = self.workspace_root.join(".sigil").join("plugins");
        if !plugin_dir.exists() {
            return Ok(());
        }
        if !plugin_dir.is_dir() {
            self.warn(
                PluginDiscoveryWarningKind::InvalidPath,
                plugin_dir,
                "plugin discovery path is not a directory",
            );
            return Ok(());
        }

        for entry in sorted_dir_entries(&plugin_dir)? {
            let plugin_root = entry.path();
            if !plugin_root.is_dir() {
                continue;
            }
            self.discover_plugin(&plugin_root, trust_entries);
        }
        Ok(())
    }

    fn discover_plugin(&mut self, plugin_root: &Path, trust_entries: &[PluginTrustEntry]) {
        let manifest_path = plugin_root.join("plugin.toml");
        if !manifest_path.is_file() {
            self.warn(
                PluginDiscoveryWarningKind::InvalidPath,
                manifest_path,
                "plugin directory is missing plugin.toml",
            );
            return;
        }

        let outcome = match self.read_manifest(plugin_root, &manifest_path) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.warn(
                    warning_kind_for_manifest_error(&error),
                    manifest_path,
                    error.to_string(),
                );
                return;
            }
        };

        let mut snapshot = PluginManifestSnapshot {
            plugin_id: outcome.manifest.id.clone(),
            name: outcome.manifest.name.clone(),
            version: outcome.manifest.version.clone(),
            description: outcome.manifest.description.clone(),
            manifest_path: display_path(&self.workspace_root, &manifest_path),
            manifest_hash: outcome.manifest_hash,
            capabilities: outcome.manifest.capabilities(),
            trust: PluginTrustDecision::NeedsReview,
        };
        if let Some(trust) = matching_trust_entry(&snapshot, trust_entries) {
            snapshot.trust = trust.decision;
        }
        if snapshot.trust == PluginTrustDecision::Trusted
            && let Err(error) = self.register_trusted_plugin(&outcome.manifest)
        {
            self.warn(
                warning_kind_for_manifest_error(&error),
                manifest_path,
                error.to_string(),
            );
            return;
        }
        self.manifests.push(snapshot);
    }

    fn read_manifest(
        &self,
        plugin_root: &Path,
        manifest_path: &Path,
    ) -> Result<PluginManifestReadOutcome> {
        let canonical_plugin_root = plugin_root
            .canonicalize()
            .with_context(|| format!("failed to resolve plugin root {}", plugin_root.display()))?;
        if !canonical_plugin_root.starts_with(&self.canonical_workspace_root) {
            bail!(
                "plugin root escapes workspace root: {}",
                plugin_root.display()
            );
        }
        let canonical_manifest = manifest_path.canonicalize().with_context(|| {
            format!(
                "failed to resolve plugin manifest {}",
                manifest_path.display()
            )
        })?;
        if !canonical_manifest.starts_with(&canonical_plugin_root) {
            bail!(
                "plugin manifest escapes plugin root: {}",
                manifest_path.display()
            );
        }

        let bytes = fs::read(manifest_path).with_context(|| {
            format!("failed to read plugin manifest {}", manifest_path.display())
        })?;
        let manifest_hash = format!("{:x}", Sha256::digest(&bytes));
        let raw = std::str::from_utf8(&bytes).with_context(|| {
            format!("plugin manifest is not utf-8: {}", manifest_path.display())
        })?;
        let mut manifest = toml::from_str::<PluginManifest>(raw)
            .with_context(|| format!("invalid plugin manifest {}", manifest_path.display()))?;
        let directory_id = plugin_root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        validate_plugin_id(&directory_id)?;
        if manifest.id != directory_id {
            bail!(
                "plugin manifest id {} does not match directory {}",
                manifest.id,
                directory_id
            );
        }
        manifest.root = plugin_root.to_path_buf();
        manifest.validate()?;
        validate_plugin_skill_paths(&manifest, &canonical_plugin_root)?;
        Ok(PluginManifestReadOutcome {
            manifest,
            manifest_hash,
        })
    }

    fn register_trusted_plugin(&mut self, manifest: &PluginManifest) -> Result<()> {
        let skills = discover_plugin_skill_descriptors(
            &self.workspace_root,
            &manifest.id,
            &manifest.root,
            &manifest.skills,
        )?;
        self.registrations.skills.extend(skills);
        self.registrations
            .hooks
            .extend(
                manifest
                    .hooks
                    .iter()
                    .cloned()
                    .map(|hook| PluginHookRegistration {
                        plugin_id: manifest.id.clone(),
                        hook,
                    }),
            );
        self.registrations
            .mcp_servers
            .extend(
                manifest
                    .mcp_servers
                    .iter()
                    .map(|server| PluginMcpServerRegistration {
                        plugin_id: manifest.id.clone(),
                        original_name: server.name.clone(),
                        server: namespaced_mcp_server(&manifest.id, server),
                    }),
            );
        Ok(())
    }

    fn warn(
        &mut self,
        kind: PluginDiscoveryWarningKind,
        path: impl AsRef<Path>,
        message: impl Into<String>,
    ) {
        self.warnings.push(PluginDiscoveryWarning {
            kind,
            path: path.as_ref().to_path_buf(),
            message: message.into(),
        });
    }

    fn finish(self) -> PluginDiscoveryReport {
        PluginDiscoveryReport {
            manifests: self.manifests,
            registrations: self.registrations,
            warnings: self.warnings,
        }
    }
}

struct PluginManifestReadOutcome {
    manifest: PluginManifest,
    manifest_hash: String,
}

fn validate_plugin_skill_paths(
    manifest: &PluginManifest,
    canonical_plugin_root: &Path,
) -> Result<()> {
    for skill in &manifest.skills {
        let path = manifest.root.join(&skill.path);
        let canonical_path = path.canonicalize().with_context(|| {
            format!(
                "failed to resolve plugin {} skill {}",
                manifest.id,
                skill.path.display()
            )
        })?;
        if !canonical_path.starts_with(canonical_plugin_root) {
            bail!(
                "plugin {} skill path escapes plugin root: {}",
                manifest.id,
                skill.path.display()
            );
        }
    }
    Ok(())
}

fn matching_trust_entry<'a>(
    snapshot: &PluginManifestSnapshot,
    trust_entries: &'a [PluginTrustEntry],
) -> Option<&'a PluginTrustEntry> {
    trust_entries
        .iter()
        .rev()
        .find(|entry| entry.matches_snapshot(snapshot))
}

fn namespaced_mcp_server(plugin_id: &str, server: &McpServerConfig) -> McpServerConfig {
    let mut server = server.clone();
    server.name = format!("{plugin_id}.{}", server.name);
    server
}

fn unique_plugin_mcp_server_name(
    plugin_id: &str,
    original_name: &str,
    used_names: &BTreeSet<String>,
) -> String {
    let identity = format!("{plugin_id}\0{original_name}");
    let hash = format!("{:x}", Sha256::digest(identity.as_bytes()));
    let mut candidate = format!("{plugin_id}.{original_name}.{}", &hash[..8]);
    let mut attempt = 0usize;
    while used_names.contains(&candidate) {
        attempt += 1;
        candidate = format!("{plugin_id}.{original_name}.{}.{attempt}", &hash[..8]);
    }
    candidate
}

fn sorted_dir_entries(dir: &Path) -> Result<Vec<fs::DirEntry>> {
    let mut entries = fs::read_dir(dir)
        .with_context(|| {
            format!(
                "failed to read plugin discovery directory {}",
                dir.display()
            )
        })?
        .filter_map(std::result::Result::ok)
        .collect::<Vec<_>>();
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries)
}

fn warning_kind_for_manifest_error(error: &anyhow::Error) -> PluginDiscoveryWarningKind {
    let message = error.to_string();
    if message.contains("failed to read") || message.contains("not utf-8") {
        PluginDiscoveryWarningKind::ReadFailed
    } else if message.contains("escapes") || message.contains("failed to resolve") {
        PluginDiscoveryWarningKind::InvalidPath
    } else {
        PluginDiscoveryWarningKind::InvalidManifest
    }
}

fn display_path(workspace_root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(workspace_root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
#[path = "tests/plugins_tests.rs"]
mod tests;
