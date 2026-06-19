use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    ControlEntry, ModelMessage, PluginSkillRef, SkillConfig, SkillDescriptor, SkillIndexSnapshot,
    SkillLoadEntry, SkillRunMode, SkillSource, SkillTrustState, Tool, ToolAccess, ToolCategory,
    ToolContext, ToolErrorKind, ToolPreviewCapability, ToolRegistry, ToolRegistryScope, ToolResult,
    ToolResultMeta, ToolSpec, ToolSubject, ToolSubjectKind, ToolSubjectScope,
};

pub const LOAD_SKILL_TOOL_NAME: &str = "load_skill";
const MAX_SKILL_BODY_BYTES: usize = 256 * 1024;
const MAX_SKILL_BODY_LINES: usize = 8_000;
const MAX_MODEL_VISIBLE_SKILLS: usize = 80;
const MAX_MODEL_VISIBLE_INDEX_BYTES: usize = 8 * 1024;

/// Result of skill discovery, including the deterministic index and non-fatal warnings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDiscoveryReport {
    pub snapshot: SkillIndexSnapshot,
    pub warnings: Vec<SkillDiscoveryWarning>,
}

/// Fully loaded skill body prepared for direct user invocation.
#[derive(Debug, Clone)]
pub struct LoadedSkillContext {
    pub descriptor: SkillDescriptor,
    pub entry: SkillLoadEntry,
    pub transient_context: ModelMessage,
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

/// Discovers plugin-owned skills from manifest-relative entries under one plugin root.
///
/// # Errors
///
/// Returns an error when a referenced skill path is unsafe, missing, unreadable, malformed, or
/// duplicated within the plugin manifest.
pub fn discover_plugin_skill_descriptors(
    workspace_root: &Path,
    plugin_id: &str,
    plugin_root: &Path,
    skills: &[PluginSkillRef],
) -> Result<Vec<SkillDescriptor>> {
    let canonical_workspace_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    let canonical_plugin_root = plugin_root
        .canonicalize()
        .with_context(|| format!("failed to resolve plugin root {}", plugin_root.display()))?;
    if !canonical_plugin_root.starts_with(&canonical_workspace_root) {
        bail!("plugin {plugin_id} root escapes workspace root");
    }

    let mut descriptors = Vec::new();
    let mut claimed_ids = BTreeSet::new();
    for skill in skills {
        let entrypoint = plugin_root.join(&skill.path);
        let canonical_entrypoint = entrypoint.canonicalize().with_context(|| {
            format!(
                "failed to resolve plugin {plugin_id} skill {}",
                skill.path.display()
            )
        })?;
        if !canonical_entrypoint.starts_with(&canonical_plugin_root) {
            bail!(
                "plugin {plugin_id} skill path escapes plugin root: {}",
                skill.path.display()
            );
        }
        let logical_entrypoint = plugin_root.join(&skill.path);
        let fallback_id = fallback_skill_id(&skill.path)?;
        let descriptor = descriptor_from_entrypoint(
            workspace_root,
            logical_entrypoint.parent().unwrap_or(plugin_root),
            &logical_entrypoint,
            &fallback_id,
            &SkillCandidateKind::PluginSkill {
                plugin_id: plugin_id.to_owned(),
            },
        )?;
        if !claimed_ids.insert(descriptor.id.clone()) {
            bail!(
                "plugin {plugin_id} declares duplicate skill {}",
                descriptor.id
            );
        }
        descriptors.push(descriptor);
    }
    Ok(descriptors)
}

/// Registers the internal model-facing skill loading tool from a discovered index.
///
/// # Errors
///
/// Returns an error when skill discovery fails.
pub fn register_skill_tools(
    registry: &mut ToolRegistry,
    workspace_root: &Path,
    user_config_dir: Option<&Path>,
    config: &SkillConfig,
) -> Result<SkillDiscoveryReport> {
    let report = discover_skill_index_with_user_dir(workspace_root, user_config_dir, config)?;
    if config.enabled {
        registry.register(Arc::new(LoadSkillTool::new(
            workspace_root.to_path_buf(),
            report.snapshot.clone(),
        )));
    }
    Ok(report)
}

#[derive(Debug, Clone)]
struct LoadSkillTool {
    workspace_root: PathBuf,
    snapshot: SkillIndexSnapshot,
}

impl LoadSkillTool {
    fn new(workspace_root: PathBuf, snapshot: SkillIndexSnapshot) -> Self {
        Self {
            workspace_root,
            snapshot,
        }
    }
}

#[async_trait]
impl Tool for LoadSkillTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: LOAD_SKILL_TOOL_NAME.to_owned(),
            description: model_visible_skill_index_description(&self.snapshot),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Stable id of a trusted model-invocable skill to load."
                    }
                },
                "required": ["id"],
                "additionalProperties": false
            }),
            category: ToolCategory::Custom,
            access: ToolAccess::Read,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_subjects(&self, _ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        Ok(vec![skill_subject(required_skill_id(args)?)])
    }

    async fn execute(&self, _ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let skill_id = match required_skill_id(&args) {
            Ok(skill_id) => skill_id.to_owned(),
            Err(error) => {
                return Ok(ToolResult::error(
                    call_id,
                    LOAD_SKILL_TOOL_NAME,
                    ToolErrorKind::InvalidInput,
                    error.to_string(),
                ));
            }
        };
        let loaded = match load_skill_context(
            &self.workspace_root,
            &self.snapshot,
            &skill_id,
            SkillLoadInvocation::Model,
            None,
            Some(call_id.clone()),
        ) {
            Ok(loaded) => loaded,
            Err(error) => {
                return Ok(ToolResult::error(
                    call_id,
                    LOAD_SKILL_TOOL_NAME,
                    error.kind,
                    error.message,
                ));
            }
        };
        Ok(ToolResult::ok(
            call_id,
            LOAD_SKILL_TOOL_NAME,
            format!(
                "loaded skill {} ({} bytes, {} lines) into transient context",
                loaded.entry.skill_id, loaded.entry.byte_count, loaded.entry.line_count
            ),
            ToolResultMeta {
                bytes: Some(loaded.entry.byte_count),
                total_lines: Some(loaded.entry.line_count),
                ..ToolResultMeta::default()
            },
        )
        .with_control_entry(ControlEntry::SkillLoaded(loaded.entry))
        .with_transient_context(vec![loaded.transient_context]))
    }
}

/// Loads a user-invoked skill body for direct TUI invocation.
///
/// # Errors
///
/// Returns an error when the skill is missing, disabled, untrusted, not user-invocable, changed
/// since discovery, outside its root, too large, or unreadable.
pub fn load_user_invoked_skill(
    workspace_root: &Path,
    snapshot: &SkillIndexSnapshot,
    skill_id: &str,
    run_id: Option<String>,
) -> Result<LoadedSkillContext> {
    load_skill_context(
        workspace_root,
        snapshot,
        skill_id,
        SkillLoadInvocation::User,
        run_id,
        None,
    )
    .map_err(|error| anyhow!(error.message))
}

#[derive(Debug, Clone, Copy)]
enum SkillLoadInvocation {
    Model,
    User,
}

#[derive(Debug)]
struct SkillLoadFailure {
    kind: ToolErrorKind,
    message: String,
}

fn load_skill_context(
    workspace_root: &Path,
    snapshot: &SkillIndexSnapshot,
    skill_id: &str,
    invocation: SkillLoadInvocation,
    run_id: Option<String>,
    call_id: Option<String>,
) -> std::result::Result<LoadedSkillContext, SkillLoadFailure> {
    let Some(descriptor) = snapshot
        .descriptors
        .iter()
        .find(|descriptor| descriptor.id == skill_id)
    else {
        return Err(skill_load_failure(
            ToolErrorKind::NotFound,
            format!("unknown skill {skill_id:?}"),
        ));
    };
    if !descriptor.enabled {
        return Err(skill_load_failure(
            ToolErrorKind::PermissionDenied,
            format!("skill {skill_id:?} is disabled"),
        ));
    }
    if descriptor.trust != SkillTrustState::Trusted {
        return Err(skill_load_failure(
            ToolErrorKind::PermissionDenied,
            format!("skill {skill_id:?} is not trusted"),
        ));
    }
    match invocation {
        SkillLoadInvocation::Model if !descriptor.model_invocable => {
            return Err(skill_load_failure(
                ToolErrorKind::PermissionDenied,
                format!("skill {skill_id:?} is not model-invocable"),
            ));
        }
        SkillLoadInvocation::User if !descriptor.user_invocable => {
            return Err(skill_load_failure(
                ToolErrorKind::PermissionDenied,
                format!("skill {skill_id:?} is not user-invocable"),
            ));
        }
        SkillLoadInvocation::Model | SkillLoadInvocation::User => {}
    }

    let root = resolved_descriptor_path(workspace_root, &descriptor.root);
    let entrypoint = resolved_descriptor_path(workspace_root, &descriptor.entrypoint);
    let root = root.canonicalize().map_err(|error| {
        skill_load_failure(
            ToolErrorKind::NotFound,
            format!("skill root cannot be resolved: {error}"),
        )
    })?;
    let entrypoint = entrypoint.canonicalize().map_err(|error| {
        skill_load_failure(
            ToolErrorKind::NotFound,
            format!("skill entrypoint cannot be resolved: {error}"),
        )
    })?;
    if !entrypoint.starts_with(&root) {
        return Err(skill_load_failure(
            ToolErrorKind::PathOutsideWorkspace,
            format!("skill {skill_id:?} entrypoint is outside its skill root"),
        ));
    }

    let bytes = fs::read(&entrypoint).map_err(|error| {
        skill_load_failure(
            ToolErrorKind::Io,
            format!("failed to read skill {skill_id:?}: {error}"),
        )
    })?;
    if bytes.len() > MAX_SKILL_BODY_BYTES {
        return Err(skill_load_failure(
            ToolErrorKind::InvalidInput,
            format!(
                "skill {skill_id:?} body is too large: {} bytes exceeds {}",
                bytes.len(),
                MAX_SKILL_BODY_BYTES
            ),
        ));
    }
    let body = std::str::from_utf8(&bytes).map_err(|error| {
        skill_load_failure(
            ToolErrorKind::Utf8,
            format!("skill {skill_id:?} is not utf-8: {error}"),
        )
    })?;
    let line_count = body.lines().count();
    if line_count > MAX_SKILL_BODY_LINES {
        return Err(skill_load_failure(
            ToolErrorKind::InvalidInput,
            format!(
                "skill {skill_id:?} body has too many lines: {line_count} exceeds {MAX_SKILL_BODY_LINES}"
            ),
        ));
    }
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    if !descriptor.sha256.is_empty() && descriptor.sha256 != sha256 {
        return Err(skill_load_failure(
            ToolErrorKind::InvalidInput,
            format!("skill {skill_id:?} hash changed since discovery"),
        ));
    }

    let entry = SkillLoadEntry {
        skill_id: descriptor.id.clone(),
        sha256,
        source: descriptor.source.clone(),
        entrypoint: descriptor.entrypoint.clone(),
        run_id,
        call_id,
        byte_count: bytes.len() as u64,
        line_count: line_count as u64,
        loaded_at_ms: unix_time_ms(),
    };
    let transient_context =
        ModelMessage::system(render_loaded_skill_context(descriptor, &entry, body));
    Ok(LoadedSkillContext {
        descriptor: descriptor.clone(),
        entry,
        transient_context,
    })
}

fn skill_load_failure(kind: ToolErrorKind, message: String) -> SkillLoadFailure {
    SkillLoadFailure { kind, message }
}

fn model_visible_skill_index_description(snapshot: &SkillIndexSnapshot) -> String {
    let mut description = String::from(
        "Load one trusted reusable skill by id. The full skill body is loaded only on demand and is injected as transient context for the current run.\n\nAvailable skills:",
    );
    let mut rendered = 0usize;
    for descriptor in snapshot
        .descriptors
        .iter()
        .filter(|descriptor| model_visible_skill_descriptor(descriptor))
        .take(MAX_MODEL_VISIBLE_SKILLS)
    {
        let mut line = format!("\n- {}", descriptor.id);
        if !descriptor.description.trim().is_empty() {
            line.push_str(": ");
            line.push_str(descriptor.description.trim());
        }
        if let Some(when_to_use) = descriptor.when_to_use.as_deref()
            && !when_to_use.trim().is_empty()
        {
            line.push_str(" Use when: ");
            line.push_str(when_to_use.trim());
        }
        if description.len() + line.len() > MAX_MODEL_VISIBLE_INDEX_BYTES {
            description.push_str("\n- ...");
            return description;
        }
        description.push_str(&line);
        rendered += 1;
    }
    if rendered == 0 {
        description.push_str("\n- none");
    }
    description
}

fn model_visible_skill_descriptor(descriptor: &SkillDescriptor) -> bool {
    descriptor.enabled && descriptor.trust == SkillTrustState::Trusted && descriptor.model_invocable
}

fn required_skill_id(args: &Value) -> Result<&str> {
    args.get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("missing id"))
}

fn skill_subject(skill_id: &str) -> ToolSubject {
    ToolSubject {
        kind: ToolSubjectKind::Other,
        original: skill_id.to_owned(),
        normalized: format!("skill:{skill_id}"),
        canonical_path: None,
        scope: ToolSubjectScope::Unknown,
    }
}

fn resolved_descriptor_path(workspace_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    }
}

fn render_loaded_skill_context(
    descriptor: &SkillDescriptor,
    entry: &SkillLoadEntry,
    body: &str,
) -> String {
    format!(
        "Loaded Sigil skill\nid: {}\nsource: {}\nrun_as: {}\nentrypoint: {}\nsha256: {}\n\n{}",
        descriptor.id,
        descriptor.source.as_str(),
        descriptor.run_as.as_str(),
        descriptor.entrypoint.display(),
        entry.sha256,
        body
    )
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
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
            self.discover_entrypoint(&path, &entrypoint, &fallback_id, &kind);
        }
    }

    fn discover_agent_dir(&mut self, dir: &Path, kind: SkillCandidateKind) {
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
            if !valid_skill_id(&fallback_id) {
                self.warn(
                    SkillDiscoveryWarningKind::InvalidName,
                    &path,
                    format!("invalid agent file name {fallback_id:?}"),
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
enum SkillCandidateKind {
    WorkspaceSkill,
    WorkspaceAgent,
    ClaudeSkill,
    ClaudeAgent,
    UserSkill,
    UserAgent,
    PluginSkill { plugin_id: String },
}

impl SkillCandidateKind {
    fn is_agent(&self) -> bool {
        matches!(
            self,
            Self::WorkspaceAgent | Self::ClaudeAgent | Self::UserAgent
        )
    }

    fn is_workspace_scoped(&self) -> bool {
        !matches!(self, Self::UserSkill | Self::UserAgent)
    }

    fn source(&self) -> SkillSource {
        match self {
            Self::WorkspaceSkill | Self::WorkspaceAgent | Self::ClaudeSkill | Self::ClaudeAgent => {
                SkillSource::Workspace
            }
            Self::UserSkill | Self::UserAgent => SkillSource::User,
            Self::PluginSkill { plugin_id } => SkillSource::Plugin {
                plugin_id: plugin_id.clone(),
            },
        }
    }

    fn default_run_mode(&self) -> SkillRunMode {
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
        kind: &SkillCandidateKind,
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
            agent: self.string("agent")?,
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
    kind: &SkillCandidateKind,
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

fn descriptor_from_entrypoint(
    workspace_root: &Path,
    root: &Path,
    entrypoint: &Path,
    fallback_id: &str,
    kind: &SkillCandidateKind,
) -> Result<SkillDescriptor> {
    let bytes = fs::read(entrypoint)
        .with_context(|| format!("failed to read skill entrypoint {}", entrypoint.display()))?;
    let raw = std::str::from_utf8(&bytes)
        .with_context(|| format!("skill entrypoint is not utf-8: {}", entrypoint.display()))?;
    let frontmatter = SkillFrontmatter::parse(raw)?;
    let id = descriptor_id(&frontmatter, fallback_id, kind)?;
    frontmatter.to_descriptor(
        id,
        root,
        entrypoint,
        fallback_id,
        format!("{:x}", Sha256::digest(&bytes)),
        kind,
        workspace_root,
    )
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

fn fallback_skill_id(path: &Path) -> Result<String> {
    if path.file_name() == Some(OsStr::new("SKILL.md"))
        && let Some(parent_name) = path.parent().and_then(Path::file_name)
    {
        let value = parent_name.to_string_lossy().into_owned();
        if valid_skill_id(&value) {
            return Ok(value);
        }
        bail!("invalid plugin skill directory name {value:?}");
    }
    let value = path
        .file_stem()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if valid_skill_id(&value) {
        Ok(value)
    } else {
        bail!("invalid plugin skill file name {value:?}")
    }
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
