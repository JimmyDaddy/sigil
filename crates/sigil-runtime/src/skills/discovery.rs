use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use anyhow::Result;
use sha2::{Digest, Sha256};
use sigil_kernel::{SkillConfig, SkillDescriptor, SkillIndexSnapshot, SkillRunMode, SkillSource};

use crate::{
    DEFAULT_PROJECT_ASSETS_DIR, DEFAULT_WORKSPACE_AGENTS_LEAF, DEFAULT_WORKSPACE_COMMANDS_LEAF,
    DEFAULT_WORKSPACE_SKILLS_LEAF, definition_file_io::prepare_definition_roots,
};

use super::{
    SkillDiscoveryReport, SkillDiscoveryWarning, SkillDiscoveryWarningKind,
    frontmatter::descriptor_from_entrypoint,
};

/// Discovers workspace skills using the configured workspace and compatibility directories.
///
/// # Errors
///
/// Returns an error if the deterministic index snapshot cannot be built.
pub fn discover_skill_index(
    workspace_root: &Path,
    config: &SkillConfig,
) -> Result<SkillDiscoveryReport> {
    discover_skill_index_with_user_dir(workspace_root, None, config)
}

/// Discovers workspace and optional user-level skills.
///
/// # Errors
///
/// Returns an error if the deterministic index snapshot cannot be built.
pub fn discover_skill_index_with_user_dir(
    workspace_root: &Path,
    user_config_dir: Option<&Path>,
    config: &SkillConfig,
) -> Result<SkillDiscoveryReport> {
    if !config.enabled {
        return Ok(SkillDiscoveryReport {
            snapshot: SkillIndexSnapshot::new(Vec::new())?,
            warnings: Vec::new(),
        });
    }

    let mut discovery = SkillDiscovery::new(workspace_root);
    let project_assets_root = workspace_root.join(DEFAULT_PROJECT_ASSETS_DIR);
    let workspace_skills = project_assets_root.join(DEFAULT_WORKSPACE_SKILLS_LEAF);
    let workspace_commands = project_assets_root.join(DEFAULT_WORKSPACE_COMMANDS_LEAF);
    let workspace_agents = project_assets_root.join(DEFAULT_WORKSPACE_AGENTS_LEAF);
    let mut definition_roots = vec![
        workspace_skills.clone(),
        workspace_commands.clone(),
        workspace_agents.clone(),
    ];
    if compatibility_source_enabled(config, "agents")
        || compatibility_source_enabled(config, "codex")
    {
        definition_roots.push(workspace_root.join(".agents").join("skills"));
    }
    if compatibility_source_enabled(config, "opencode") {
        let root = workspace_root.join(".opencode");
        definition_roots.extend([
            root.join("skills"),
            root.join("commands"),
            root.join("agents"),
        ]);
    }
    if compatibility_source_enabled(config, "claude") {
        let root = workspace_root.join(".claude");
        definition_roots.extend([
            root.join("skills"),
            root.join("commands"),
            root.join("agents"),
        ]);
    }
    if compatibility_source_enabled(config, "reasonix") {
        definition_roots.push(workspace_root.join(".reasonix").join("agents"));
    }
    prepare_definition_roots(&definition_roots);

    discovery.discover_skill_dir(&workspace_skills, SkillCandidateKind::WorkspaceSkill);
    discovery.discover_markdown_file_dir(
        &workspace_commands,
        SkillCandidateKind::WorkspaceCommand,
        "command",
    );
    discovery.discover_agent_dir(&workspace_agents, SkillCandidateKind::WorkspaceAgent);

    if compatibility_source_enabled(config, "agents")
        || compatibility_source_enabled(config, "codex")
    {
        discovery.discover_skill_dir(
            &workspace_root.join(".agents").join("skills"),
            SkillCandidateKind::AgentStandardSkill,
        );
    }
    if compatibility_source_enabled(config, "opencode") {
        let root = workspace_root.join(".opencode");
        discovery.discover_skill_dir(&root.join("skills"), SkillCandidateKind::OpenCodeSkill);
        discovery.discover_markdown_file_dir(
            &root.join("commands"),
            SkillCandidateKind::OpenCodeCommand,
            "command",
        );
        discovery.discover_agent_dir(&root.join("agents"), SkillCandidateKind::OpenCodeAgent);
    }
    if compatibility_source_enabled(config, "claude") {
        let root = workspace_root.join(".claude");
        discovery.discover_skill_dir(&root.join("skills"), SkillCandidateKind::ClaudeSkill);
        discovery.discover_markdown_file_dir(
            &root.join("commands"),
            SkillCandidateKind::ClaudeCommand,
            "command",
        );
        discovery.discover_agent_dir(&root.join("agents"), SkillCandidateKind::ClaudeAgent);
    }
    if compatibility_source_enabled(config, "reasonix") {
        discovery.discover_agent_dir(
            &workspace_root.join(".reasonix").join("agents"),
            SkillCandidateKind::ReasonixAgent,
        );
    }

    if let Some(user_config_dir) = user_config_dir {
        if config.user_skills {
            discovery.discover_skill_dir(
                &user_config_dir.join("skills"),
                SkillCandidateKind::UserSkill,
            );
        }
        if config.user_agents {
            discovery.discover_agent_dir(
                &user_config_dir.join("agents"),
                SkillCandidateKind::UserAgent,
            );
        }
    }

    discovery.finish()
}

struct SkillDiscovery {
    workspace_root: PathBuf,
    canonical_workspace_root: PathBuf,
    descriptors: Vec<SkillDescriptor>,
    claimed_ids: BTreeMap<String, PathBuf>,
    warnings: Vec<SkillDiscoveryWarning>,
}

impl SkillDiscovery {
    fn new(workspace_root: &Path) -> Self {
        let canonical_workspace_root = workspace_root
            .canonicalize()
            .unwrap_or_else(|_| workspace_root.to_path_buf());
        Self {
            workspace_root: workspace_root.to_path_buf(),
            canonical_workspace_root,
            descriptors: Vec::new(),
            claimed_ids: BTreeMap::new(),
            warnings: Vec::new(),
        }
    }

    fn discover_skill_dir(&mut self, dir: &Path, kind: SkillCandidateKind) {
        if !self.directory_is_valid(dir, &kind) {
            return;
        }

        for entry in self.sorted_dir_entries(dir) {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let fallback_id = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            if !valid_skill_id(&fallback_id) && !kind.supports_derived_id() {
                self.warn(
                    SkillDiscoveryWarningKind::InvalidName,
                    &path,
                    format!("invalid skill directory name {fallback_id:?}"),
                );
                continue;
            }
            let entrypoint = path.join("SKILL.md");
            if !entrypoint.is_file() {
                self.warn(
                    SkillDiscoveryWarningKind::InvalidPath,
                    &entrypoint,
                    "skill directory is missing SKILL.md",
                );
                continue;
            }
            self.discover_entrypoint(&path, &entrypoint, &fallback_id, &kind);
        }
    }

    fn discover_agent_dir(&mut self, dir: &Path, kind: SkillCandidateKind) {
        self.discover_markdown_file_dir(dir, kind, "agent");
    }

    fn discover_markdown_file_dir(&mut self, dir: &Path, kind: SkillCandidateKind, noun: &str) {
        if !self.directory_is_valid(dir, &kind) {
            return;
        }

        for entry in self.sorted_dir_entries(dir) {
            let path = entry.path();
            if path.extension().and_then(OsStr::to_str) != Some("md") {
                continue;
            }
            let fallback_id = path
                .file_stem()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            if !valid_skill_id(&fallback_id) && !kind.supports_derived_id() {
                self.warn(
                    SkillDiscoveryWarningKind::InvalidName,
                    &path,
                    format!("invalid {noun} file name {fallback_id:?}"),
                );
                continue;
            }
            let root = path.parent().unwrap_or(dir);
            self.discover_entrypoint(root, &path, &fallback_id, &kind);
        }
    }

    fn discover_entrypoint(
        &mut self,
        root: &Path,
        entrypoint: &Path,
        fallback_id: &str,
        kind: &SkillCandidateKind,
    ) {
        if !self.entrypoint_is_valid(entrypoint, kind) {
            return;
        }

        let descriptor = match descriptor_from_entrypoint(
            &self.workspace_root,
            root,
            entrypoint,
            fallback_id,
            kind,
        ) {
            Ok(descriptor) => descriptor,
            Err(error) => {
                self.warn(
                    warning_kind_for_descriptor_error(&error),
                    entrypoint,
                    error.to_string(),
                );
                return;
            }
        };
        let id = descriptor.id.clone();
        if let Some(existing_path) = self.claimed_ids.get(&id) {
            self.warn(
                SkillDiscoveryWarningKind::Shadowed,
                entrypoint,
                format!("skill id {id:?} is shadowed by {}", existing_path.display()),
            );
            return;
        }
        self.claimed_ids.insert(id, descriptor.entrypoint.clone());
        self.descriptors.push(descriptor);
    }

    fn directory_is_valid(&mut self, dir: &Path, kind: &SkillCandidateKind) -> bool {
        if !dir.exists() {
            return false;
        }
        if !dir.is_dir() {
            self.warn(
                SkillDiscoveryWarningKind::InvalidPath,
                dir,
                "skill discovery path is not a directory",
            );
            return false;
        }
        if kind.is_workspace_scoped() && !self.path_stays_in_workspace(dir) {
            self.warn(
                SkillDiscoveryWarningKind::InvalidPath,
                dir,
                "workspace skill discovery path escapes workspace root",
            );
            return false;
        }
        true
    }

    fn entrypoint_is_valid(&mut self, entrypoint: &Path, kind: &SkillCandidateKind) -> bool {
        if kind.is_workspace_scoped() && !self.path_stays_in_workspace(entrypoint) {
            self.warn(
                SkillDiscoveryWarningKind::InvalidPath,
                entrypoint,
                "workspace skill entrypoint escapes workspace root",
            );
            return false;
        }
        true
    }

    fn path_stays_in_workspace(&self, path: &Path) -> bool {
        path.canonicalize()
            .map(|canonical| canonical.starts_with(&self.canonical_workspace_root))
            .unwrap_or(false)
    }

    fn sorted_dir_entries(&mut self, dir: &Path) -> Vec<fs::DirEntry> {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => {
                self.warn(
                    SkillDiscoveryWarningKind::ReadFailed,
                    dir,
                    format!("failed to read skill discovery directory: {error}"),
                );
                return Vec::new();
            }
        };
        let mut entries = entries.filter_map(|entry| entry.ok()).collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        entries
    }

    fn warn(
        &mut self,
        kind: SkillDiscoveryWarningKind,
        path: impl AsRef<Path>,
        message: impl Into<String>,
    ) {
        self.warnings.push(SkillDiscoveryWarning {
            kind,
            path: path.as_ref().to_path_buf(),
            message: message.into(),
        });
    }

    fn finish(self) -> Result<SkillDiscoveryReport> {
        Ok(SkillDiscoveryReport {
            snapshot: SkillIndexSnapshot::new(self.descriptors)?,
            warnings: self.warnings,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SkillCandidateKind {
    WorkspaceSkill,
    WorkspaceCommand,
    WorkspaceAgent,
    AgentStandardSkill,
    OpenCodeSkill,
    OpenCodeCommand,
    OpenCodeAgent,
    ClaudeSkill,
    ClaudeCommand,
    ClaudeAgent,
    ReasonixAgent,
    UserSkill,
    UserAgent,
    PluginSkill { plugin_id: String },
}

impl SkillCandidateKind {
    fn is_agent(&self) -> bool {
        matches!(
            self,
            Self::WorkspaceAgent
                | Self::OpenCodeAgent
                | Self::ClaudeAgent
                | Self::ReasonixAgent
                | Self::UserAgent
        )
    }

    pub(super) fn supports_derived_id(&self) -> bool {
        matches!(
            self,
            Self::OpenCodeCommand
                | Self::OpenCodeAgent
                | Self::ClaudeCommand
                | Self::ClaudeAgent
                | Self::ReasonixAgent
        )
    }

    pub(super) fn is_foreign_compatibility(&self) -> bool {
        matches!(
            self,
            Self::AgentStandardSkill
                | Self::OpenCodeSkill
                | Self::OpenCodeCommand
                | Self::OpenCodeAgent
                | Self::ClaudeSkill
                | Self::ClaudeCommand
                | Self::ClaudeAgent
                | Self::ReasonixAgent
        )
    }

    pub(super) fn derived_id_prefix(&self) -> &'static str {
        if self.is_agent() {
            "compat-agent"
        } else {
            "compat-command"
        }
    }

    fn is_workspace_scoped(&self) -> bool {
        !matches!(self, Self::UserSkill | Self::UserAgent)
    }

    pub(super) fn source(&self) -> SkillSource {
        match self {
            Self::WorkspaceSkill
            | Self::WorkspaceCommand
            | Self::WorkspaceAgent
            | Self::AgentStandardSkill
            | Self::OpenCodeSkill
            | Self::OpenCodeCommand
            | Self::OpenCodeAgent
            | Self::ClaudeSkill
            | Self::ClaudeCommand
            | Self::ClaudeAgent
            | Self::ReasonixAgent => SkillSource::Workspace,
            Self::UserSkill | Self::UserAgent => SkillSource::User,
            Self::PluginSkill { plugin_id } => SkillSource::Plugin {
                plugin_id: plugin_id.clone(),
            },
        }
    }

    pub(super) fn default_run_mode(&self) -> SkillRunMode {
        if self.is_agent() {
            SkillRunMode::ChildSession
        } else {
            SkillRunMode::Inline
        }
    }
}

fn warning_kind_for_descriptor_error(error: &anyhow::Error) -> SkillDiscoveryWarningKind {
    let message = error.to_string();
    if message.contains("invalid skill id") {
        SkillDiscoveryWarningKind::InvalidName
    } else if message.contains("failed to read") || message.contains("not utf-8") {
        SkillDiscoveryWarningKind::ReadFailed
    } else {
        SkillDiscoveryWarningKind::InvalidFrontmatter
    }
}

pub(crate) fn compatibility_source_enabled(config: &SkillConfig, source: &str) -> bool {
    const DEFAULT_SOURCES: [&str; 4] = ["agents", "claude", "codex", "opencode"];
    if config.compatibility_auto_discover
        && DEFAULT_SOURCES
            .iter()
            .any(|default| default.eq_ignore_ascii_case(source))
    {
        return true;
    }
    config
        .compatibility_sources
        .iter()
        .any(|configured| configured.trim().eq_ignore_ascii_case(source))
}

pub(crate) fn compatibility_stable_id(prefix: &str, logical_name: &str) -> String {
    if valid_skill_id(logical_name) {
        return logical_name.to_owned();
    }
    let digest = format!("{:x}", Sha256::digest(logical_name.trim().as_bytes()));
    format!("{prefix}-{}", &digest[..16])
}

pub(super) fn valid_skill_id(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphanumeric())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}
