use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    SkillConfig, SkillDescriptor, SkillIndexSnapshot, SkillRunMode, SkillSource, SkillTrustState,
    ToolRegistryScope,
};

/// Result of skill discovery, including the deterministic index and non-fatal warnings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDiscoveryReport {
    pub snapshot: SkillIndexSnapshot,
    pub warnings: Vec<SkillDiscoveryWarning>,
}

/// One non-fatal problem found while discovering skills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDiscoveryWarning {
    pub kind: SkillDiscoveryWarningKind,
    pub path: PathBuf,
    pub message: String,
}

/// Stable warning categories for discovery diagnostics and future TUI display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillDiscoveryWarningKind {
    InvalidPath,
    InvalidName,
    InvalidFrontmatter,
    ReadFailed,
    Shadowed,
}

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
    let workspace_skills = configured_dir(workspace_root, &config.workspace_dir);
    discovery.discover_skill_dir(&workspace_skills, SkillCandidateKind::WorkspaceSkill);

    let workspace_agents = configured_dir(workspace_root, &config.workspace_agents_dir);
    discovery.discover_agent_dir(&workspace_agents, SkillCandidateKind::WorkspaceAgent);

    if compatibility_source_enabled(config, "claude") {
        discovery.discover_skill_dir(
            &workspace_root.join(".claude").join("skills"),
            SkillCandidateKind::ClaudeSkill,
        );
        discovery.discover_agent_dir(
            &workspace_root.join(".claude").join("agents"),
            SkillCandidateKind::ClaudeAgent,
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

/// Builds the stable runtime id for a plugin-provided skill.
///
/// # Errors
///
/// Returns an error when either segment is not a valid skill id segment.
pub fn namespaced_plugin_skill_id(plugin_id: &str, skill_id: &str) -> Result<String> {
    if !valid_skill_id(plugin_id) {
        bail!("invalid plugin id {plugin_id:?}");
    }
    if !valid_skill_id(skill_id) {
        bail!("invalid plugin skill id {skill_id:?}");
    }
    Ok(format!("{plugin_id}/{skill_id}"))
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
        if !self.directory_is_valid(dir, kind) {
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
            if !valid_skill_id(&fallback_id) {
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
            self.discover_entrypoint(&path, &entrypoint, &fallback_id, kind);
        }
    }

    fn discover_agent_dir(&mut self, dir: &Path, kind: SkillCandidateKind) {
        if !self.directory_is_valid(dir, kind) {
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
            if !valid_skill_id(&fallback_id) {
                self.warn(
                    SkillDiscoveryWarningKind::InvalidName,
                    &path,
                    format!("invalid agent file name {fallback_id:?}"),
                );
                continue;
            }
            let root = path.parent().unwrap_or(dir);
            self.discover_entrypoint(root, &path, &fallback_id, kind);
        }
    }

    fn discover_entrypoint(
        &mut self,
        root: &Path,
        entrypoint: &Path,
        fallback_id: &str,
        kind: SkillCandidateKind,
    ) {
        if !self.entrypoint_is_valid(entrypoint, kind) {
            return;
        }

        let bytes = match fs::read(entrypoint) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.warn(
                    SkillDiscoveryWarningKind::ReadFailed,
                    entrypoint,
                    format!("failed to read skill entrypoint: {error}"),
                );
                return;
            }
        };
        let raw = match std::str::from_utf8(&bytes) {
            Ok(raw) => raw,
            Err(error) => {
                self.warn(
                    SkillDiscoveryWarningKind::ReadFailed,
                    entrypoint,
                    format!("skill entrypoint is not utf-8: {error}"),
                );
                return;
            }
        };
        let frontmatter = match SkillFrontmatter::parse(raw) {
            Ok(frontmatter) => frontmatter,
            Err(error) => {
                self.warn(
                    SkillDiscoveryWarningKind::InvalidFrontmatter,
                    entrypoint,
                    error.to_string(),
                );
                return;
            }
        };
        let id = match descriptor_id(&frontmatter, fallback_id, kind) {
            Ok(id) => id,
            Err(error) => {
                self.warn(
                    SkillDiscoveryWarningKind::InvalidName,
                    entrypoint,
                    error.to_string(),
                );
                return;
            }
        };
        if let Some(existing_path) = self.claimed_ids.get(&id) {
            self.warn(
                SkillDiscoveryWarningKind::Shadowed,
                entrypoint,
                format!("skill id {id:?} is shadowed by {}", existing_path.display()),
            );
            return;
        }

        let descriptor = match frontmatter.to_descriptor(
            id.clone(),
            root,
            entrypoint,
            fallback_id,
            format!("{:x}", Sha256::digest(&bytes)),
            kind,
            &self.workspace_root,
        ) {
            Ok(descriptor) => descriptor,
            Err(error) => {
                self.warn(
                    SkillDiscoveryWarningKind::InvalidFrontmatter,
                    entrypoint,
                    error.to_string(),
                );
                return;
            }
        };
        self.claimed_ids.insert(id, descriptor.entrypoint.clone());
        self.descriptors.push(descriptor);
    }

    fn directory_is_valid(&mut self, dir: &Path, kind: SkillCandidateKind) -> bool {
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

    fn entrypoint_is_valid(&mut self, entrypoint: &Path, kind: SkillCandidateKind) -> bool {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkillCandidateKind {
    WorkspaceSkill,
    WorkspaceAgent,
    ClaudeSkill,
    ClaudeAgent,
    UserSkill,
    UserAgent,
}

impl SkillCandidateKind {
    fn is_agent(self) -> bool {
        matches!(
            self,
            Self::WorkspaceAgent | Self::ClaudeAgent | Self::UserAgent
        )
    }

    fn is_workspace_scoped(self) -> bool {
        !matches!(self, Self::UserSkill | Self::UserAgent)
    }

    fn source(self) -> SkillSource {
        match self {
            Self::WorkspaceSkill | Self::WorkspaceAgent | Self::ClaudeSkill | Self::ClaudeAgent => {
                SkillSource::Workspace
            }
            Self::UserSkill | Self::UserAgent => SkillSource::User,
        }
    }

    fn default_run_mode(self) -> SkillRunMode {
        if self.is_agent() {
            SkillRunMode::ChildSession
        } else {
            SkillRunMode::Inline
        }
    }
}

#[derive(Debug, Clone, Default)]
struct SkillFrontmatter {
    fields: BTreeMap<String, FrontmatterField>,
}

impl SkillFrontmatter {
    fn parse(raw: &str) -> Result<Self> {
        let mut lines = raw.lines();
        let Some(first) = lines.next() else {
            return Ok(Self::default());
        };
        if first.trim_end_matches('\r') != "---" {
            return Ok(Self::default());
        }

        let mut frontmatter_lines = Vec::new();
        let mut closed = false;
        for line in lines {
            if line.trim_end_matches('\r') == "---" {
                closed = true;
                break;
            }
            frontmatter_lines.push(line.trim_end_matches('\r').to_owned());
        }
        if !closed {
            bail!("unterminated skill frontmatter");
        }

        let fields = parse_frontmatter_fields(&frontmatter_lines)?;
        Ok(Self { fields })
    }

    fn to_descriptor(
        &self,
        id: String,
        root: &Path,
        entrypoint: &Path,
        fallback_id: &str,
        sha256: String,
        kind: SkillCandidateKind,
        workspace_root: &Path,
    ) -> Result<SkillDescriptor> {
        let name = self
            .string("name")?
            .or(self.string("id")?)
            .unwrap_or_else(|| fallback_id.to_owned());
        let model_invocable = !self.bool("disable_model_invocation")?.unwrap_or(false);
        let allowed_tools = self
            .string_list("allowed_tools")?
            .or(self.string_list("tools")?)
            .unwrap_or_default();

        Ok(SkillDescriptor {
            id,
            name,
            description: self.string("description")?.unwrap_or_default(),
            when_to_use: self.string("when_to_use")?,
            root: display_path(workspace_root, root),
            entrypoint: display_path(workspace_root, entrypoint),
            source: kind.source(),
            sha256,
            enabled: self.bool("enabled")?.unwrap_or(true),
            trust: self.trust_state()?.unwrap_or_default(),
            model_invocable,
            user_invocable: self.bool("user_invocable")?.unwrap_or(true),
            run_as: self.run_mode()?.unwrap_or_else(|| kind.default_run_mode()),
            argument_hint: self.string("argument_hint")?,
            allowed_tools: tool_scope_from_items(allowed_tools),
            disallowed_tools: tool_scope_from_items(
                self.string_list("disallowed_tools")?.unwrap_or_default(),
            ),
            path_patterns: self.string_list("paths")?.unwrap_or_default(),
        })
    }

    fn string(&self, key: &str) -> Result<Option<String>> {
        let Some(field) = self.fields.get(key) else {
            return Ok(None);
        };
        field
            .value
            .clone()
            .filter(|value| !value.trim().is_empty())
            .map(Ok)
            .transpose()
    }

    fn bool(&self, key: &str) -> Result<Option<bool>> {
        self.string(key)?
            .map(|value| match normalized_scalar(&value).as_str() {
                "true" | "yes" => Ok(true),
                "false" | "no" => Ok(false),
                other => Err(anyhow!("invalid boolean value {other:?} for {key}")),
            })
            .transpose()
    }

    fn string_list(&self, key: &str) -> Result<Option<Vec<String>>> {
        let Some(field) = self.fields.get(key) else {
            return Ok(None);
        };
        if !field.list.is_empty() {
            return Ok(Some(field.list.clone()));
        }
        let Some(value) = field.value.as_deref() else {
            return Ok(Some(Vec::new()));
        };
        parse_inline_list(value)
            .with_context(|| format!("invalid list value for {key}"))
            .map(Some)
    }

    fn run_mode(&self) -> Result<Option<SkillRunMode>> {
        self.string("run_as")?
            .map(|value| match normalized_scalar(&value).as_str() {
                "inline" => Ok(SkillRunMode::Inline),
                "child_session" | "child" | "subagent" | "agent" => Ok(SkillRunMode::ChildSession),
                other => Err(anyhow!("invalid run-as value {other:?}")),
            })
            .transpose()
    }

    fn trust_state(&self) -> Result<Option<SkillTrustState>> {
        self.string("trust")?
            .map(|value| match normalized_scalar(&value).as_str() {
                "trusted" | "trust" => Ok(SkillTrustState::Trusted),
                "needs_review" | "review" => Ok(SkillTrustState::NeedsReview),
                "disabled" | "disable" => Ok(SkillTrustState::Disabled),
                other => Err(anyhow!("invalid trust value {other:?}")),
            })
            .transpose()
    }
}

#[derive(Debug, Clone, Default)]
struct FrontmatterField {
    value: Option<String>,
    list: Vec<String>,
}

fn parse_frontmatter_fields(lines: &[String]) -> Result<BTreeMap<String, FrontmatterField>> {
    let mut fields = BTreeMap::new();
    let mut index = 0;
    while index < lines.len() {
        let line = &lines[index];
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('#')
            || line.starts_with(' ')
            || line.starts_with('\t')
        {
            index += 1;
            continue;
        }
        let Some((raw_key, raw_value)) = line.split_once(':') else {
            bail!("unsupported frontmatter line {line:?}");
        };
        let key = normalize_key(raw_key);
        if key.is_empty() {
            bail!("empty frontmatter key");
        }
        let value = raw_value.trim();
        if value == ">" || value == "|" {
            bail!("unsupported multiline frontmatter value for {key}");
        }
        if value.is_empty() {
            let mut list = Vec::new();
            index += 1;
            let expects_list = list_frontmatter_key(&key);
            while index < lines.len() {
                let nested = &lines[index];
                if nested.trim().is_empty() {
                    index += 1;
                    continue;
                }
                if !nested.starts_with(' ') && !nested.starts_with('\t') {
                    break;
                }
                let nested_trimmed = nested.trim_start();
                if let Some(item) = nested_trimmed.strip_prefix("- ") {
                    list.push(clean_scalar(item)?);
                } else if expects_list {
                    bail!("unsupported list item for {key}: {nested_trimmed:?}");
                }
                index += 1;
            }
            fields.insert(key, FrontmatterField { value: None, list });
            continue;
        }

        fields.insert(
            key,
            FrontmatterField {
                value: Some(clean_scalar(value)?),
                list: Vec::new(),
            },
        );
        index += 1;
    }
    Ok(fields)
}

fn descriptor_id(
    frontmatter: &SkillFrontmatter,
    fallback_id: &str,
    kind: SkillCandidateKind,
) -> Result<String> {
    let base_id = frontmatter
        .string("id")?
        .or(frontmatter.string("name")?)
        .unwrap_or_else(|| fallback_id.to_owned());
    if !valid_skill_id(&base_id) {
        bail!("invalid skill id {base_id:?}");
    }
    if let SkillSource::Plugin { plugin_id } = kind.source() {
        return namespaced_plugin_skill_id(&plugin_id, &base_id);
    }
    Ok(base_id)
}

fn configured_dir(base: &Path, configured: &str) -> PathBuf {
    let path = PathBuf::from(configured.trim());
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

fn compatibility_source_enabled(config: &SkillConfig, source: &str) -> bool {
    config
        .compatibility_sources
        .iter()
        .any(|configured| configured.trim().eq_ignore_ascii_case(source))
}

fn display_path(workspace_root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(workspace_root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| path.to_path_buf())
}

fn valid_skill_id(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphanumeric())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

fn normalize_key(raw_key: &str) -> String {
    raw_key.trim().replace('-', "_").to_ascii_lowercase()
}

fn list_frontmatter_key(key: &str) -> bool {
    matches!(
        key,
        "allowed_tools" | "tools" | "disallowed_tools" | "paths"
    )
}

fn normalized_scalar(value: &str) -> String {
    value.trim().replace('-', "_").to_ascii_lowercase()
}

fn clean_scalar(value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed == ">" || trimmed == "|" {
        bail!("unsupported multiline scalar");
    }
    let unquoted = if quoted_scalar(trimmed) {
        &trimmed[1..trimmed.len() - 1]
    } else {
        strip_comment(trimmed).trim()
    };
    Ok(unquoted.trim().to_owned())
}

fn quoted_scalar(value: &str) -> bool {
    value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
}

fn strip_comment(value: &str) -> &str {
    value
        .split_once(" #")
        .map(|(value, _comment)| value)
        .unwrap_or(value)
}

fn parse_inline_list(value: &str) -> Result<Vec<String>> {
    let trimmed = value.trim();
    let inner = if trimmed.starts_with('[') {
        if !trimmed.ends_with(']') {
            bail!("unterminated bracket list");
        }
        &trimmed[1..trimmed.len().saturating_sub(1)]
    } else {
        trimmed
    };
    if inner.trim().is_empty() {
        return Ok(Vec::new());
    }
    Ok(inner
        .split(',')
        .map(clean_scalar)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .filter(|item| !item.is_empty())
        .collect())
}

fn tool_scope_from_items(items: Vec<String>) -> ToolRegistryScope {
    let mut allow_all = false;
    let mut names = BTreeSet::new();
    let mut prefixes = Vec::new();
    for item in items {
        let trimmed = item.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "*" || trimmed.eq_ignore_ascii_case("all") {
            allow_all = true;
        } else if let Some(prefix) = trimmed.strip_suffix('*') {
            prefixes.push(prefix.to_owned());
        } else {
            names.insert(trimmed.to_owned());
        }
    }
    ToolRegistryScope {
        allow_all,
        names,
        prefixes,
    }
}

#[cfg(test)]
#[path = "tests/skills_tests.rs"]
mod tests;
