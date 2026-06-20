use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    AgentInvocationPolicy, AgentProfile, AgentProfileId, AgentProfileKind,
    AgentProfilePolicyProjection, AgentProfileSnapshot, AgentProfileSnapshotId, AgentProfileSource,
    AgentProfileTrustProjection, AgentResultPolicy, AgentRole, AgentTrustState,
    PluginStateProjection, ReasoningEffort, RootConfig, SessionLogEntry, SkillDescriptor,
    SkillRunMode, SkillSource, SkillTrustState, ToolRegistryScope,
};

use crate::{
    LOAD_SKILL_TOOL_NAME, plugins::discover_workspace_plugins, provider_config_key,
    skills::discover_skill_index,
};

pub const BUILD_PROFILE_ID: &str = "build";
pub const PLAN_PROFILE_ID: &str = "plan";
pub const EXPLORE_PROFILE_ID: &str = "explore";
pub const WORKER_PROFILE_ID: &str = "worker";

/// Resolved runtime profile plus discovery metadata that is not part of the durable kernel type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAgentProfile {
    pub profile: AgentProfile,
    pub enabled: bool,
    pub enabled_override: Option<bool>,
    pub user_invocable_override: Option<bool>,
    pub model_invocable_override: Option<bool>,
    pub source: AgentProfileSource,
    pub source_hash: String,
    pub trust_state: AgentTrustState,
}

impl ResolvedAgentProfile {
    #[must_use]
    pub fn id(&self) -> &AgentProfileId {
        &self.profile.id
    }

    #[must_use]
    pub fn effective_enabled(&self) -> bool {
        self.enabled_override.unwrap_or(self.enabled)
    }

    #[must_use]
    pub fn effective_user_invocation_allowed(&self) -> bool {
        self.user_invocable_override
            .unwrap_or_else(|| self.profile.user_invocation_allowed())
    }

    #[must_use]
    pub fn effective_model_invocation_allowed(&self) -> bool {
        self.model_invocable_override
            .unwrap_or_else(|| self.profile.model_invocation_allowed())
    }
}

/// Deterministic model-visible projection of trusted invocable agent profiles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelVisibleAgentIndex {
    pub entries: Vec<ModelVisibleAgentIndexEntry>,
    pub hidden_count: usize,
    pub fingerprint: String,
}

/// One bounded profile row visible to model-facing spawn-agent descriptions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelVisibleAgentIndexEntry {
    pub profile_id: AgentProfileId,
    pub kind: AgentProfileKind,
    pub description: String,
    pub result_policy: AgentResultPolicy,
    pub tool_scope_hash: String,
}

/// Current run constraints used to filter the model-visible agent index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProfileIndexContext {
    pub tool_scope: ToolRegistryScope,
    pub allowed_profile_ids: Option<BTreeSet<AgentProfileId>>,
    pub max_entries: Option<usize>,
}

impl Default for AgentProfileIndexContext {
    fn default() -> Self {
        Self {
            tool_scope: ToolRegistryScope {
                allow_all: true,
                ..ToolRegistryScope::default()
            },
            allowed_profile_ids: None,
            max_entries: None,
        }
    }
}

/// Deterministic runtime registry for built-in and discovered agent profiles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProfileRegistry {
    profiles: Vec<ResolvedAgentProfile>,
    warnings: Vec<String>,
}

impl AgentProfileRegistry {
    /// Builds the first runtime profile registry from existing task role configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if a built-in profile id or captured profile hash cannot be produced.
    pub fn from_root_config(root_config: &RootConfig) -> Result<Self> {
        Self::from_root_config_inner(root_config, None, None)
    }

    /// Builds the registry and overlays durable profile trust decisions from session entries.
    ///
    /// # Errors
    ///
    /// Returns an error if profile discovery, stable hashing, or trust projection application fails.
    pub fn from_root_config_with_entries(
        root_config: &RootConfig,
        entries: &[SessionLogEntry],
    ) -> Result<Self> {
        let mut registry = Self::from_root_config_inner(root_config, None, Some(entries))?;
        registry.apply_session_entry_projections(entries)?;
        Ok(registry)
    }

    /// Builds the runtime profile registry from config plus native workspace agent profiles.
    ///
    /// Native workspace profiles are read from the configured `.sigil/agents` directory using the
    /// caller-resolved workspace root. This keeps profile discovery out of config parsing and makes
    /// path trust checks explicit at the runtime boundary.
    ///
    /// # Errors
    ///
    /// Returns an error if built-in profile construction or stable hashing fails.
    pub fn from_root_config_with_workspace(
        root_config: &RootConfig,
        workspace_root: &Path,
    ) -> Result<Self> {
        Self::from_root_config_inner(root_config, Some(workspace_root), None)
    }

    /// Builds the workspace-aware registry and overlays durable profile trust decisions.
    ///
    /// # Errors
    ///
    /// Returns an error if profile discovery, stable hashing, or trust projection application fails.
    pub fn from_root_config_with_workspace_and_entries(
        root_config: &RootConfig,
        workspace_root: &Path,
        entries: &[SessionLogEntry],
    ) -> Result<Self> {
        let mut registry =
            Self::from_root_config_inner(root_config, Some(workspace_root), Some(entries))?;
        registry.apply_session_entry_projections(entries)?;
        Ok(registry)
    }

    fn from_root_config_inner(
        root_config: &RootConfig,
        workspace_root: Option<&Path>,
        entries: Option<&[SessionLogEntry]>,
    ) -> Result<Self> {
        let mut profiles = vec![
            builtin_profile(
                root_config,
                BuiltinProfileSpec {
                    id: BUILD_PROFILE_ID,
                    kind: AgentProfileKind::Primary,
                    role: AgentRole::Executor,
                    description: "Default coding agent.",
                    instructions: "Handle ordinary coding tasks in the current session.",
                    enabled: true,
                    invocation_policy: AgentInvocationPolicy::ManualOnly,
                    result_policy: AgentResultPolicy::SummaryWithPageRef,
                    nickname_candidates: &[],
                },
            )?,
            builtin_profile(
                root_config,
                BuiltinProfileSpec {
                    id: PLAN_PROFILE_ID,
                    kind: AgentProfileKind::Primary,
                    role: AgentRole::Planner,
                    description: "Analysis-first planning agent.",
                    instructions: "Research and plan before execution; do not edit files directly.",
                    enabled: root_config.task.enabled,
                    invocation_policy: AgentInvocationPolicy::ManualOnly,
                    result_policy: AgentResultPolicy::SummaryOnly,
                    nickname_candidates: &[],
                },
            )?,
            builtin_profile(
                root_config,
                BuiltinProfileSpec {
                    id: EXPLORE_PROFILE_ID,
                    kind: AgentProfileKind::Subagent,
                    role: AgentRole::SubagentRead,
                    description: "Read-only codebase exploration and verification agent.",
                    instructions: "Inspect the repository with read-only tools and return a bounded result.",
                    enabled: root_config.task.enabled,
                    invocation_policy: AgentInvocationPolicy::ModelAllowed,
                    result_policy: AgentResultPolicy::SummaryWithPageRef,
                    nickname_candidates: &["Atlas", "Delta", "Echo"],
                },
            )?,
            builtin_profile(
                root_config,
                BuiltinProfileSpec {
                    id: WORKER_PROFILE_ID,
                    kind: AgentProfileKind::Subagent,
                    role: AgentRole::SubagentWrite,
                    description: "Foreground implementation worker agent.",
                    instructions: "Perform implementation work only through the guarded foreground path.",
                    enabled: root_config.task.enabled,
                    invocation_policy: AgentInvocationPolicy::ManualOnly,
                    result_policy: AgentResultPolicy::ForegroundMergeRequired,
                    nickname_candidates: &["Patch", "Forge", "Quill"],
                },
            )?,
        ];
        let mut warnings = Vec::new();
        if let Some(workspace_root) = workspace_root {
            discover_workspace_agent_profiles(
                root_config,
                workspace_root,
                &mut profiles,
                &mut warnings,
            )?;
            discover_plugin_agent_profiles(
                root_config,
                workspace_root,
                entries.unwrap_or(&[]),
                &mut profiles,
                &mut warnings,
            )?;
            discover_child_session_skill_profiles(
                root_config,
                workspace_root,
                &mut profiles,
                &mut warnings,
            )?;
        }
        profiles.sort_by(|left, right| left.profile.id.cmp(&right.profile.id));
        disable_conflicting_profile_names(&mut profiles, &mut warnings);
        Ok(Self { profiles, warnings })
    }

    #[must_use]
    pub fn profiles(&self) -> &[ResolvedAgentProfile] {
        &self.profiles
    }

    #[must_use]
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    #[must_use]
    pub fn get(&self, profile_id: &AgentProfileId) -> Option<&ResolvedAgentProfile> {
        self.profiles
            .iter()
            .find(|profile| &profile.profile.id == profile_id)
    }

    /// Captures an immutable snapshot for a resolved profile.
    ///
    /// # Errors
    ///
    /// Returns an error if stable JSON hashing or id validation fails.
    pub fn capture_snapshot(&self, profile_id: &AgentProfileId) -> Result<AgentProfileSnapshot> {
        let resolved = self
            .get(profile_id)
            .with_context(|| format!("agent profile {} is not registered", profile_id.as_str()))?;
        capture_profile_snapshot(resolved)
    }

    /// Applies durable trust decisions to non-system profiles.
    ///
    /// A matching source/profile hash replays the reviewed decision. If an older decision exists for
    /// the same profile id but the current snapshot no longer matches, the profile returns to review
    /// state so source edits cannot silently retain stale trust.
    ///
    /// # Errors
    ///
    /// Returns an error if a profile snapshot cannot be captured for stable hash comparison.
    pub fn apply_profile_trust_projection(
        &mut self,
        projection: &AgentProfileTrustProjection,
    ) -> Result<()> {
        for profile in &mut self.profiles {
            if profile.source == AgentProfileSource::System {
                continue;
            }
            let snapshot = capture_profile_snapshot(profile)?;
            if let Some(decision) = projection.decision_for_snapshot(&snapshot) {
                profile.trust_state = decision;
            } else if projection.has_decision_for_profile(&profile.profile.id) {
                profile.trust_state = AgentTrustState::NeedsReview;
            }
        }
        Ok(())
    }

    /// Applies durable user policy overrides to non-system profiles.
    ///
    /// Matching source/profile hashes replay the user's enabled/user/model invocability decisions
    /// without mutating the source [`AgentProfile`]. Stale decisions are cleared so profile edits do
    /// not silently retain old policy.
    ///
    /// # Errors
    ///
    /// Returns an error if a profile snapshot cannot be captured for stable hash comparison.
    pub fn apply_profile_policy_projection(
        &mut self,
        projection: &AgentProfilePolicyProjection,
    ) -> Result<()> {
        for profile in &mut self.profiles {
            if profile.source == AgentProfileSource::System {
                continue;
            }
            let snapshot = capture_profile_snapshot(profile)?;
            if let Some(policy) = projection.policy_for_snapshot(&snapshot) {
                profile.enabled_override = policy.enabled;
                profile.user_invocable_override = policy.user_invocable;
                profile.model_invocable_override = policy.model_invocable;
            } else if projection.has_policy_for_profile(&profile.profile.id) {
                profile.enabled_override = None;
                profile.user_invocable_override = None;
                profile.model_invocable_override = None;
            }
        }
        Ok(())
    }

    fn apply_session_entry_projections(&mut self, entries: &[SessionLogEntry]) -> Result<()> {
        let trust_projection = AgentProfileTrustProjection::from_entries(entries);
        self.apply_profile_trust_projection(&trust_projection)?;
        let policy_projection = AgentProfilePolicyProjection::from_entries(entries);
        self.apply_profile_policy_projection(&policy_projection)
    }

    /// Builds a deterministic model-visible index for the current run constraints.
    ///
    /// # Errors
    ///
    /// Returns an error if stable profile hashing fails.
    pub fn model_visible_index(
        &self,
        context: &AgentProfileIndexContext,
    ) -> Result<ModelVisibleAgentIndex> {
        let mut candidates = self
            .profiles
            .iter()
            .filter(|profile| profile.effective_enabled())
            .filter(|profile| profile.trust_state == AgentTrustState::Trusted)
            .filter(|profile| profile.effective_model_invocation_allowed())
            .filter(|profile| profile_allowed_by_context(profile, context))
            .map(|profile| {
                Ok(ModelVisibleAgentIndexEntry {
                    profile_id: profile.profile.id.clone(),
                    kind: profile.profile.kind,
                    description: profile.profile.description.clone(),
                    result_policy: profile.profile.result_policy,
                    tool_scope_hash: hash_json(&serde_json::to_value(
                        &profile.profile.tool_scope,
                    )?)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        candidates.sort_by(|left, right| left.profile_id.cmp(&right.profile_id));
        let total = candidates.len();
        if let Some(max_entries) = context.max_entries {
            candidates.truncate(max_entries);
        }
        let hidden_count = total.saturating_sub(candidates.len());
        let fingerprint = model_visible_fingerprint(&candidates, hidden_count, context)?;
        Ok(ModelVisibleAgentIndex {
            entries: candidates,
            hidden_count,
            fingerprint,
        })
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
struct NativeAgentProfileWire {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    kind: Option<AgentProfileKind>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<ReasoningEffort>,
    #[serde(default)]
    tool_scope: Option<ToolRegistryScope>,
    #[serde(default)]
    allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    tools: Option<Vec<String>>,
    #[serde(default)]
    invocation_policy: Option<AgentInvocationPolicy>,
    #[serde(default)]
    result_policy: Option<AgentResultPolicy>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    trust: Option<AgentTrustState>,
    #[serde(default)]
    trust_state: Option<AgentTrustState>,
    #[serde(default)]
    user_invocable: Option<bool>,
    #[serde(default)]
    model_invocable: Option<bool>,
    #[serde(default)]
    skills: Option<Vec<String>>,
    #[serde(default)]
    mcp_servers: Option<Vec<String>>,
    #[serde(default)]
    nickname_candidates: Option<Vec<String>>,
    #[serde(default)]
    aliases: Option<Vec<String>>,
    #[serde(default)]
    slash_names: Option<Vec<String>>,
}

fn discover_workspace_agent_profiles(
    root_config: &RootConfig,
    workspace_root: &Path,
    profiles: &mut Vec<ResolvedAgentProfile>,
    warnings: &mut Vec<String>,
) -> Result<()> {
    if !root_config.skills.enabled {
        return Ok(());
    }
    let agents_dir = configured_dir(workspace_root, &root_config.skills.workspace_agents_dir);
    if !agents_dir.exists() {
        return Ok(());
    }
    if !agents_dir.is_dir() {
        warnings.push(format!(
            "workspace agent discovery path is not a directory: {}",
            agents_dir.display()
        ));
        return Ok(());
    }
    let canonical_workspace = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    if !path_stays_in_workspace(&canonical_workspace, &agents_dir) {
        warnings.push(format!(
            "workspace agent discovery path escapes workspace root: {}",
            agents_dir.display()
        ));
        return Ok(());
    }

    let mut claimed_ids = profiles
        .iter()
        .map(|profile| {
            (
                profile.profile.id.as_str().to_owned(),
                agent_profile_source_label(&profile.source).to_owned(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for entry in sorted_dir_entries(&agents_dir, warnings) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let fallback_id = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Err(error) = AgentProfileId::new(fallback_id.clone()) {
            warnings.push(format!(
                "invalid workspace agent directory name {}: {error}",
                path.display()
            ));
            continue;
        }
        if !path_stays_in_workspace(&canonical_workspace, &path) {
            warnings.push(format!(
                "workspace agent profile path escapes workspace root: {}",
                path.display()
            ));
            continue;
        }
        let Some((entrypoint, format)) = native_agent_entrypoint(&path) else {
            continue;
        };
        if !path_stays_in_workspace(&canonical_workspace, &entrypoint) {
            warnings.push(format!(
                "workspace agent profile entrypoint escapes workspace root: {}",
                entrypoint.display()
            ));
            continue;
        }
        let raw = match fs::read_to_string(&entrypoint) {
            Ok(raw) => raw,
            Err(error) => {
                warnings.push(format!(
                    "failed to read workspace agent profile {}: {error}",
                    entrypoint.display()
                ));
                continue;
            }
        };
        let resolved = match workspace_agent_profile_from_raw(
            root_config,
            workspace_root,
            &path,
            &entrypoint,
            &fallback_id,
            &raw,
            format,
        ) {
            Ok(profile) => profile,
            Err(error) => {
                warnings.push(format!(
                    "invalid workspace agent profile {}: {error}",
                    entrypoint.display()
                ));
                continue;
            }
        };
        let id = resolved.profile.id.as_str().to_owned();
        if let Some(existing) = claimed_ids.get(&id) {
            warnings.push(format!(
                "workspace agent profile id {id:?} from {} is shadowed by {existing}",
                entrypoint.display()
            ));
            continue;
        }
        claimed_ids.insert(
            id,
            display_path(workspace_root, &entrypoint)
                .display()
                .to_string(),
        );
        profiles.push(resolved);
    }
    Ok(())
}

fn discover_plugin_agent_profiles(
    root_config: &RootConfig,
    workspace_root: &Path,
    entries: &[SessionLogEntry],
    profiles: &mut Vec<ResolvedAgentProfile>,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let plugin_projection = PluginStateProjection::from_entries(entries);
    let trust_entries = plugin_projection
        .trust_entries
        .into_values()
        .collect::<Vec<_>>();
    let report = discover_workspace_plugins(workspace_root, &trust_entries)?;
    warnings.extend(report.warnings.into_iter().map(|warning| {
        format!(
            "plugin discovery warning while projecting agent profiles: {}: {}",
            warning.path.display(),
            warning.message
        )
    }));
    let mut claimed_ids = profiles
        .iter()
        .map(|profile| {
            (
                profile.profile.id.as_str().to_owned(),
                agent_profile_source_label(&profile.source).to_owned(),
            )
        })
        .collect::<BTreeMap<_, _>>();

    for registration in report.registrations.agents {
        let entrypoint = registration.plugin_root.join(&registration.agent.path);
        let format = match plugin_agent_profile_format(&entrypoint) {
            Ok(format) => format,
            Err(error) => {
                warnings.push(format!(
                    "invalid plugin agent profile {} from plugin {}: {error}",
                    registration.agent.path.display(),
                    registration.plugin_id
                ));
                continue;
            }
        };
        let fallback_id = match fallback_plugin_agent_id(&registration.agent.path) {
            Ok(id) => id,
            Err(error) => {
                warnings.push(format!(
                    "invalid plugin agent profile {} from plugin {}: {error}",
                    registration.agent.path.display(),
                    registration.plugin_id
                ));
                continue;
            }
        };
        let raw = match fs::read_to_string(&entrypoint) {
            Ok(raw) => raw,
            Err(error) => {
                warnings.push(format!(
                    "failed to read plugin agent profile {} from plugin {}: {error}",
                    registration.agent.path.display(),
                    registration.plugin_id
                ));
                continue;
            }
        };
        let resolved = match plugin_agent_profile_from_raw(
            root_config,
            workspace_root,
            &registration.plugin_id,
            &registration.plugin_root,
            &entrypoint,
            &fallback_id,
            &raw,
            format,
        ) {
            Ok(profile) => profile,
            Err(error) => {
                warnings.push(format!(
                    "invalid plugin agent profile {} from plugin {}: {error}",
                    registration.agent.path.display(),
                    registration.plugin_id
                ));
                continue;
            }
        };
        let id = resolved.profile.id.as_str().to_owned();
        if let Some(existing) = claimed_ids.get(&id) {
            warnings.push(format!(
                "plugin agent profile id {id:?} from {} is shadowed by {existing}",
                display_path(workspace_root, &entrypoint).display()
            ));
            continue;
        }
        claimed_ids.insert(
            id,
            display_path(workspace_root, &entrypoint)
                .display()
                .to_string(),
        );
        profiles.push(resolved);
    }
    Ok(())
}

fn discover_child_session_skill_profiles(
    root_config: &RootConfig,
    workspace_root: &Path,
    profiles: &mut Vec<ResolvedAgentProfile>,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let report = discover_skill_index(workspace_root, &root_config.skills)?;
    warnings.extend(report.warnings.into_iter().map(|warning| {
        format!(
            "skill discovery warning while projecting agent profiles: {}: {}",
            warning.path.display(),
            warning.message
        )
    }));
    let mut claimed_ids = profiles
        .iter()
        .map(|profile| {
            (
                profile.profile.id.as_str().to_owned(),
                agent_profile_source_label(&profile.source).to_owned(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for descriptor in report
        .snapshot
        .descriptors
        .iter()
        .filter(|descriptor| descriptor.run_as == SkillRunMode::ChildSession)
    {
        if !tool_scope_is_empty(&descriptor.disallowed_tools) {
            warnings.push(format!(
                "child-session skill {:?} cannot be projected as an agent profile because disallowed_tools cannot be represented safely",
                descriptor.id
            ));
            continue;
        }
        let resolved = match child_session_skill_profile(root_config, workspace_root, descriptor) {
            Ok(profile) => profile,
            Err(error) => {
                warnings.push(format!(
                    "invalid child-session skill agent profile {:?}: {error:#}",
                    descriptor.id
                ));
                continue;
            }
        };
        let id = resolved.profile.id.as_str().to_owned();
        if let Some(existing) = claimed_ids.get(&id) {
            warnings.push(format!(
                "child-session skill agent profile id {id:?} from {} is shadowed by {existing}",
                descriptor.entrypoint.display()
            ));
            continue;
        }
        claimed_ids.insert(
            id,
            display_path(
                workspace_root,
                &workspace_path(workspace_root, &descriptor.entrypoint),
            )
            .display()
            .to_string(),
        );
        profiles.push(resolved);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeAgentProfileFormat {
    Toml,
    Markdown,
}

fn native_agent_entrypoint(dir: &Path) -> Option<(PathBuf, NativeAgentProfileFormat)> {
    let toml = dir.join("agent.toml");
    if toml.is_file() {
        return Some((toml, NativeAgentProfileFormat::Toml));
    }
    let markdown = dir.join("AGENT.md");
    markdown
        .is_file()
        .then_some((markdown, NativeAgentProfileFormat::Markdown))
}

fn plugin_agent_profile_format(entrypoint: &Path) -> Result<NativeAgentProfileFormat> {
    match entrypoint.file_name().and_then(|name| name.to_str()) {
        Some("agent.toml") => Ok(NativeAgentProfileFormat::Toml),
        Some("AGENT.md") => Ok(NativeAgentProfileFormat::Markdown),
        Some(name) if name.ends_with(".toml") => Ok(NativeAgentProfileFormat::Toml),
        Some(name) if name.ends_with(".md") => Ok(NativeAgentProfileFormat::Markdown),
        _ => bail!("plugin agent path must point to agent.toml, AGENT.md, .toml, or .md"),
    }
}

fn fallback_plugin_agent_id(path: &Path) -> Result<String> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let fallback = if matches!(file_name, "agent.toml" | "AGENT.md") {
        path.parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or("")
    } else {
        path.file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("")
    };
    AgentProfileId::new(fallback.to_owned())
        .with_context(|| format!("invalid plugin agent fallback id {fallback:?}"))?;
    Ok(fallback.to_owned())
}

fn namespaced_plugin_agent_profile_id(plugin_id: &str, local_id: &str) -> Result<AgentProfileId> {
    let plugin_segment = plugin_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    let candidate = format!("{plugin_segment}-{local_id}");
    if candidate.len() <= 96 {
        return AgentProfileId::new(candidate);
    }

    let hash = hash_bytes(format!("{plugin_id}\0{local_id}").as_bytes());
    let prefix = format!("plugin-{}-", &hash[..12]);
    let max_local_len = 96usize.saturating_sub(prefix.len());
    let local_part = &local_id[..local_id.len().min(max_local_len)];
    AgentProfileId::new(format!("{prefix}{local_part}"))
}

fn workspace_agent_profile_from_raw(
    root_config: &RootConfig,
    workspace_root: &Path,
    root: &Path,
    entrypoint: &Path,
    fallback_id: &str,
    raw: &str,
    format: NativeAgentProfileFormat,
) -> Result<ResolvedAgentProfile> {
    let (wire, markdown_body) = match format {
        NativeAgentProfileFormat::Toml => (toml::from_str::<NativeAgentProfileWire>(raw)?, None),
        NativeAgentProfileFormat::Markdown => markdown_agent_profile_wire(raw)?,
    };
    let id = wire.id.as_deref().unwrap_or(fallback_id);
    if id != fallback_id {
        bail!("agent profile id {id:?} must match directory name {fallback_id:?}");
    }
    let profile_id = AgentProfileId::new(id.to_owned())?;
    let invocation_policy = wire.invocation_policy.unwrap_or_else(|| {
        AgentInvocationPolicy::from_invocability(
            wire.user_invocable.unwrap_or(true),
            wire.model_invocable.unwrap_or(false),
        )
    });
    let user_invocable = wire
        .user_invocable
        .unwrap_or_else(|| invocation_policy.default_user_invocable());
    let model_invocable = wire
        .model_invocable
        .unwrap_or_else(|| invocation_policy.default_model_invocable());
    let tool_scope = wire
        .tool_scope
        .or_else(|| {
            wire.allowed_tools
                .clone()
                .or_else(|| wire.tools.clone())
                .map(|tools| {
                    ToolRegistryScope::from_names_and_prefixes(tools, Vec::<String>::new())
                })
        })
        .unwrap_or_else(read_only_role_tool_scope);
    let instructions = wire
        .instructions
        .or(markdown_body)
        .unwrap_or_default()
        .trim()
        .to_owned();
    let aliases = normalize_profile_name_list(wire.aliases.unwrap_or_default(), "agent alias")?;
    let slash_names =
        normalize_profile_name_list(wire.slash_names.unwrap_or_default(), "agent slash name")?;
    let profile = AgentProfile {
        id: profile_id,
        kind: wire.kind.unwrap_or(AgentProfileKind::Subagent),
        description: wire.description.unwrap_or_default(),
        instructions,
        model: wire.model.or_else(|| Some(root_config.agent.model.clone())),
        provider: wire
            .provider
            .or_else(|| Some(root_config.agent.provider.clone())),
        reasoning_effort: wire.reasoning_effort,
        tool_scope,
        permission_policy: root_config.permission.clone(),
        invocation_policy,
        result_policy: wire.result_policy.unwrap_or_default(),
        user_invocable,
        model_invocable,
        skills: wire.skills.unwrap_or_default(),
        mcp_servers: wire.mcp_servers.unwrap_or_default(),
        nickname_candidates: wire.nickname_candidates.unwrap_or_default(),
        aliases,
        slash_names,
    };
    Ok(ResolvedAgentProfile {
        source_hash: hash_json(&json!({
            "kind": "workspace_agent_profile",
            "root": display_path(workspace_root, root),
            "entrypoint": display_path(workspace_root, entrypoint),
            "sha256": hash_bytes(raw.as_bytes()),
        }))?,
        profile,
        enabled: wire.enabled.unwrap_or(true),
        enabled_override: None,
        user_invocable_override: None,
        model_invocable_override: None,
        source: AgentProfileSource::Workspace,
        trust_state: wire.trust.or(wire.trust_state).unwrap_or_default(),
    })
}

fn plugin_agent_profile_from_raw(
    root_config: &RootConfig,
    workspace_root: &Path,
    plugin_id: &str,
    plugin_root: &Path,
    entrypoint: &Path,
    fallback_id: &str,
    raw: &str,
    format: NativeAgentProfileFormat,
) -> Result<ResolvedAgentProfile> {
    let (wire, markdown_body) = match format {
        NativeAgentProfileFormat::Toml => (toml::from_str::<NativeAgentProfileWire>(raw)?, None),
        NativeAgentProfileFormat::Markdown => markdown_agent_profile_wire(raw)?,
    };
    let local_id = wire.id.as_deref().unwrap_or(fallback_id);
    if local_id != fallback_id {
        bail!("agent profile id {local_id:?} must match file-derived id {fallback_id:?}");
    }
    let profile_id = namespaced_plugin_agent_profile_id(plugin_id, local_id)?;
    let invocation_policy = wire.invocation_policy.unwrap_or_else(|| {
        AgentInvocationPolicy::from_invocability(
            wire.user_invocable.unwrap_or(true),
            wire.model_invocable.unwrap_or(false),
        )
    });
    let user_invocable = wire
        .user_invocable
        .unwrap_or_else(|| invocation_policy.default_user_invocable());
    let model_invocable = wire
        .model_invocable
        .unwrap_or_else(|| invocation_policy.default_model_invocable());
    let tool_scope = wire
        .tool_scope
        .or_else(|| {
            wire.allowed_tools
                .clone()
                .or_else(|| wire.tools.clone())
                .map(|tools| {
                    ToolRegistryScope::from_names_and_prefixes(tools, Vec::<String>::new())
                })
        })
        .unwrap_or_else(read_only_role_tool_scope);
    let instructions = wire
        .instructions
        .or(markdown_body)
        .unwrap_or_default()
        .trim()
        .to_owned();
    let aliases = normalize_profile_name_list(wire.aliases.unwrap_or_default(), "agent alias")?;
    let slash_names =
        normalize_profile_name_list(wire.slash_names.unwrap_or_default(), "agent slash name")?;
    let profile = AgentProfile {
        id: profile_id,
        kind: wire.kind.unwrap_or(AgentProfileKind::Subagent),
        description: wire.description.unwrap_or_default(),
        instructions,
        model: wire.model.or_else(|| Some(root_config.agent.model.clone())),
        provider: wire
            .provider
            .or_else(|| Some(root_config.agent.provider.clone())),
        reasoning_effort: wire.reasoning_effort,
        tool_scope,
        permission_policy: root_config.permission.clone(),
        invocation_policy,
        result_policy: wire.result_policy.unwrap_or_default(),
        user_invocable,
        model_invocable,
        skills: wire.skills.unwrap_or_default(),
        mcp_servers: wire.mcp_servers.unwrap_or_default(),
        nickname_candidates: wire.nickname_candidates.unwrap_or_default(),
        aliases,
        slash_names,
    };
    Ok(ResolvedAgentProfile {
        source_hash: hash_json(&json!({
            "kind": "plugin_agent_profile",
            "plugin_id": plugin_id,
            "root": display_path(workspace_root, plugin_root),
            "entrypoint": display_path(workspace_root, entrypoint),
            "sha256": hash_bytes(raw.as_bytes()),
        }))?,
        profile,
        enabled: wire.enabled.unwrap_or(true),
        enabled_override: None,
        user_invocable_override: None,
        model_invocable_override: None,
        source: AgentProfileSource::Plugin {
            plugin_id: plugin_id.to_owned(),
        },
        trust_state: wire.trust.or(wire.trust_state).unwrap_or_default(),
    })
}

fn child_session_skill_profile(
    root_config: &RootConfig,
    workspace_root: &Path,
    descriptor: &SkillDescriptor,
) -> Result<ResolvedAgentProfile> {
    let profile_id = AgentProfileId::new(descriptor.id.clone())?;
    let invocation_policy = AgentInvocationPolicy::from_invocability(
        descriptor.user_invocable,
        descriptor.model_invocable,
    );
    let tool_scope = if tool_scope_is_empty(&descriptor.allowed_tools) {
        read_only_role_tool_scope()
    } else {
        descriptor.allowed_tools.clone()
    };
    let entrypoint = workspace_path(workspace_root, &descriptor.entrypoint);
    let raw = fs::read_to_string(&entrypoint).with_context(|| {
        format!(
            "failed to read child-session skill {}",
            entrypoint.display()
        )
    })?;
    let instructions = compatibility_skill_instructions(descriptor, &raw);
    let profile = AgentProfile {
        id: profile_id,
        kind: AgentProfileKind::Subagent,
        description: descriptor.description.clone(),
        instructions,
        model: Some(root_config.agent.model.clone()),
        provider: Some(root_config.agent.provider.clone()),
        reasoning_effort: None,
        tool_scope,
        permission_policy: root_config.permission.clone(),
        invocation_policy,
        result_policy: AgentResultPolicy::SummaryWithPageRef,
        user_invocable: descriptor.user_invocable,
        model_invocable: descriptor.model_invocable,
        skills: vec![descriptor.id.clone()],
        mcp_servers: Vec::new(),
        nickname_candidates: if descriptor.name.trim().is_empty() {
            Vec::new()
        } else {
            vec![descriptor.name.clone()]
        },
        aliases: Vec::new(),
        slash_names: Vec::new(),
    };
    Ok(ResolvedAgentProfile {
        source_hash: hash_json(&json!({
            "kind": "child_session_skill_agent_profile",
            "skill_id": descriptor.id,
            "entrypoint": display_path(workspace_root, &entrypoint),
            "sha256": descriptor.sha256,
            "run_as": descriptor.run_as.as_str(),
            "model_invocable": descriptor.model_invocable,
            "user_invocable": descriptor.user_invocable,
        }))?,
        profile,
        enabled: descriptor.enabled,
        enabled_override: None,
        user_invocable_override: None,
        model_invocable_override: None,
        source: agent_profile_source_from_skill(descriptor),
        trust_state: agent_trust_from_skill(descriptor.trust),
    })
}

fn compatibility_skill_instructions(descriptor: &SkillDescriptor, raw: &str) -> String {
    let mut parts = Vec::new();
    if !descriptor.description.trim().is_empty() {
        parts.push(format!("Description: {}", descriptor.description.trim()));
    }
    if let Some(when_to_use) = descriptor.when_to_use.as_deref()
        && !when_to_use.trim().is_empty()
    {
        parts.push(format!("When to use: {}", when_to_use.trim()));
    }
    let body = markdown_body_without_frontmatter(raw).trim().to_owned();
    if !body.is_empty() {
        parts.push(body);
    }
    parts.join("\n\n")
}

fn markdown_body_without_frontmatter(raw: &str) -> &str {
    let Some(rest) = raw.strip_prefix("---") else {
        return raw;
    };
    let rest = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))
        .unwrap_or(rest);
    if let Some((_, body)) = rest.split_once("\n---\n") {
        return body;
    }
    if let Some((_, body)) = rest.split_once("\r\n---\r\n") {
        return body;
    }
    raw
}

fn agent_profile_source_from_skill(descriptor: &SkillDescriptor) -> AgentProfileSource {
    let entrypoint = descriptor.entrypoint.to_string_lossy();
    if entrypoint.starts_with(".claude/") || entrypoint.starts_with(".claude\\") {
        return AgentProfileSource::Compatibility {
            provider: "claude".to_owned(),
        };
    }
    if entrypoint.starts_with(".reasonix/") || entrypoint.starts_with(".reasonix\\") {
        return AgentProfileSource::Compatibility {
            provider: "reasonix".to_owned(),
        };
    }
    match &descriptor.source {
        SkillSource::Workspace => AgentProfileSource::Workspace,
        SkillSource::User => AgentProfileSource::User,
        SkillSource::Plugin { plugin_id } => AgentProfileSource::Plugin {
            plugin_id: plugin_id.clone(),
        },
    }
}

fn agent_trust_from_skill(trust: SkillTrustState) -> AgentTrustState {
    match trust {
        SkillTrustState::Trusted => AgentTrustState::Trusted,
        SkillTrustState::NeedsReview => AgentTrustState::NeedsReview,
        SkillTrustState::Disabled => AgentTrustState::Disabled,
    }
}

fn markdown_agent_profile_wire(raw: &str) -> Result<(NativeAgentProfileWire, Option<String>)> {
    let mut lines = raw.lines();
    let Some(first) = lines.next() else {
        return Ok((NativeAgentProfileWire::default(), None));
    };
    if first.trim_end_matches('\r') != "---" {
        return Ok((NativeAgentProfileWire::default(), Some(raw.to_owned())));
    }
    let mut frontmatter = Vec::new();
    let mut body = Vec::new();
    let mut closed = false;
    for line in lines.by_ref() {
        if line.trim_end_matches('\r') == "---" {
            closed = true;
            break;
        }
        frontmatter.push(line.trim_end_matches('\r').to_owned());
    }
    if !closed {
        bail!("unterminated agent frontmatter");
    }
    body.extend(lines.map(str::to_owned));
    let fields = parse_markdown_frontmatter_fields(&frontmatter)?;
    Ok((wire_from_frontmatter_fields(fields)?, Some(body.join("\n"))))
}

fn parse_markdown_frontmatter_fields(lines: &[String]) -> Result<BTreeMap<String, Vec<String>>> {
    let mut fields = BTreeMap::<String, Vec<String>>::new();
    let mut current_key: Option<String> = None;
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(item) = trimmed.strip_prefix("- ") {
            let Some(key) = current_key.as_ref() else {
                bail!("frontmatter list item without a key");
            };
            fields
                .entry(key.clone())
                .or_default()
                .push(strip_scalar_quotes(item.trim()).to_owned());
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            bail!("invalid frontmatter line {trimmed:?}");
        };
        let key = key.trim().replace('-', "_");
        current_key = Some(key.clone());
        let value = value.trim();
        if value.is_empty() {
            fields.entry(key).or_default();
        } else {
            fields.insert(key, parse_inline_values(value));
        }
    }
    Ok(fields)
}

fn wire_from_frontmatter_fields(
    fields: BTreeMap<String, Vec<String>>,
) -> Result<NativeAgentProfileWire> {
    let string = |key: &str| -> Option<String> {
        fields
            .get(key)
            .and_then(|values| values.first())
            .filter(|value| !value.trim().is_empty())
            .cloned()
    };
    let list = |key: &str| -> Option<Vec<String>> {
        fields.get(key).cloned().filter(|values| !values.is_empty())
    };
    Ok(NativeAgentProfileWire {
        id: string("id"),
        kind: string("kind")
            .map(|value| parse_agent_kind(&value))
            .transpose()?,
        description: string("description"),
        instructions: string("instructions"),
        model: string("model"),
        provider: string("provider"),
        reasoning_effort: string("reasoning_effort")
            .map(|value| parse_reasoning_effort(&value))
            .transpose()?,
        tool_scope: None,
        allowed_tools: list("allowed_tools"),
        tools: list("tools"),
        invocation_policy: string("invocation_policy")
            .map(|value| parse_invocation_policy(&value))
            .transpose()?,
        result_policy: string("result_policy")
            .map(|value| parse_result_policy(&value))
            .transpose()?,
        enabled: string("enabled")
            .map(|value| parse_bool(&value))
            .transpose()?,
        trust: string("trust")
            .map(|value| parse_trust_state(&value))
            .transpose()?,
        trust_state: string("trust_state")
            .map(|value| parse_trust_state(&value))
            .transpose()?,
        user_invocable: string("user_invocable")
            .map(|value| parse_bool(&value))
            .transpose()?,
        model_invocable: string("model_invocable")
            .map(|value| parse_bool(&value))
            .transpose()?,
        skills: list("skills"),
        mcp_servers: list("mcp_servers"),
        nickname_candidates: list("nickname_candidates"),
        aliases: list("aliases").or_else(|| list("alias")),
        slash_names: list("slash_names").or_else(|| list("slash_name")),
    })
}

fn parse_bool(value: &str) -> Result<bool> {
    match normalized_scalar(value).as_str() {
        "true" | "yes" => Ok(true),
        "false" | "no" => Ok(false),
        other => Err(anyhow!("invalid boolean value {other:?}")),
    }
}

fn parse_agent_kind(value: &str) -> Result<AgentProfileKind> {
    match normalized_scalar(value).as_str() {
        "primary" => Ok(AgentProfileKind::Primary),
        "subagent" | "child" | "agent" => Ok(AgentProfileKind::Subagent),
        "system" => Ok(AgentProfileKind::System),
        other => Err(anyhow!("invalid agent kind {other:?}")),
    }
}

fn parse_invocation_policy(value: &str) -> Result<AgentInvocationPolicy> {
    match normalized_scalar(value).as_str() {
        "manual_only" | "manual" => Ok(AgentInvocationPolicy::ManualOnly),
        "model_allowed" | "model" => Ok(AgentInvocationPolicy::ModelAllowed),
        "system_only" | "system" => Ok(AgentInvocationPolicy::SystemOnly),
        other => Err(anyhow!("invalid invocation policy {other:?}")),
    }
}

fn parse_result_policy(value: &str) -> Result<AgentResultPolicy> {
    match normalized_scalar(value).as_str() {
        "summary_only" => Ok(AgentResultPolicy::SummaryOnly),
        "summary_with_page_ref" | "summary" => Ok(AgentResultPolicy::SummaryWithPageRef),
        "artifact_only" | "artifact" => Ok(AgentResultPolicy::ArtifactOnly),
        "foreground_merge_required" | "foreground" => {
            Ok(AgentResultPolicy::ForegroundMergeRequired)
        }
        other => Err(anyhow!("invalid result policy {other:?}")),
    }
}

fn parse_trust_state(value: &str) -> Result<AgentTrustState> {
    match normalized_scalar(value).as_str() {
        "trusted" | "trust" => Ok(AgentTrustState::Trusted),
        "needs_review" | "review" => Ok(AgentTrustState::NeedsReview),
        "disabled" | "disable" => Ok(AgentTrustState::Disabled),
        other => Err(anyhow!("invalid trust state {other:?}")),
    }
}

fn parse_reasoning_effort(value: &str) -> Result<ReasoningEffort> {
    match normalized_scalar(value).as_str() {
        "low" => Ok(ReasoningEffort::Low),
        "medium" => Ok(ReasoningEffort::Medium),
        "high" => Ok(ReasoningEffort::High),
        "max" => Ok(ReasoningEffort::Max),
        other => Err(anyhow!("invalid reasoning effort {other:?}")),
    }
}

fn parse_inline_values(value: &str) -> Vec<String> {
    let trimmed = value.trim();
    if let Some(inner) = trimmed
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    {
        return inner
            .split(',')
            .map(|item| strip_scalar_quotes(item.trim()).to_owned())
            .filter(|item| !item.is_empty())
            .collect();
    }
    vec![strip_scalar_quotes(trimmed).to_owned()]
}

fn strip_scalar_quotes(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(value)
}

fn normalized_scalar(value: &str) -> String {
    strip_scalar_quotes(value)
        .trim()
        .to_ascii_lowercase()
        .replace('-', "_")
}

fn configured_dir(workspace_root: &Path, configured: &str) -> PathBuf {
    let path = Path::new(configured);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    }
}

fn workspace_path(workspace_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    }
}

fn path_stays_in_workspace(canonical_workspace_root: &Path, path: &Path) -> bool {
    path.canonicalize()
        .map(|canonical| canonical.starts_with(canonical_workspace_root))
        .unwrap_or(false)
}

fn sorted_dir_entries(dir: &Path, warnings: &mut Vec<String>) -> Vec<fs::DirEntry> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            warnings.push(format!(
                "failed to read workspace agent discovery directory {}: {error}",
                dir.display()
            ));
            return Vec::new();
        }
    };
    let mut entries = entries.filter_map(|entry| entry.ok()).collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    entries
}

fn display_path(workspace_root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(workspace_root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| path.to_path_buf())
}

fn agent_profile_source_label(source: &AgentProfileSource) -> &'static str {
    match source {
        AgentProfileSource::Workspace => "workspace",
        AgentProfileSource::User => "user",
        AgentProfileSource::Plugin { .. } => "plugin",
        AgentProfileSource::Compatibility { .. } => "compatibility",
        AgentProfileSource::System => "system",
        AgentProfileSource::LegacyTask => "legacy_task",
        AgentProfileSource::Unknown => "unknown",
    }
}

fn normalize_profile_name_list(values: Vec<String>, label: &str) -> Result<Vec<String>> {
    let mut names = BTreeSet::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        let name = trimmed
            .strip_prefix('@')
            .or_else(|| trimmed.strip_prefix('/'))
            .unwrap_or(trimmed)
            .trim();
        AgentProfileId::new(name.to_owned())
            .with_context(|| format!("invalid {label} {value:?}"))?;
        names.insert(name.to_owned());
    }
    Ok(names.into_iter().collect())
}

fn disable_conflicting_profile_names(
    profiles: &mut [ResolvedAgentProfile],
    warnings: &mut Vec<String>,
) {
    disable_conflicting_profile_name_kind(
        profiles,
        warnings,
        ProfileNameKind::Alias,
        |profile| &profile.profile.aliases,
        |profile| &mut profile.profile.aliases,
    );
    disable_conflicting_profile_name_kind(
        profiles,
        warnings,
        ProfileNameKind::SlashName,
        |profile| &profile.profile.slash_names,
        |profile| &mut profile.profile.slash_names,
    );
}

#[derive(Debug, Clone, Copy)]
enum ProfileNameKind {
    Alias,
    SlashName,
}

impl ProfileNameKind {
    fn label(self) -> &'static str {
        match self {
            Self::Alias => "alias",
            Self::SlashName => "slash name",
        }
    }
}

fn disable_conflicting_profile_name_kind(
    profiles: &mut [ResolvedAgentProfile],
    warnings: &mut Vec<String>,
    kind: ProfileNameKind,
    names: fn(&ResolvedAgentProfile) -> &Vec<String>,
    names_mut: fn(&mut ResolvedAgentProfile) -> &mut Vec<String>,
) {
    let profile_ids = profiles
        .iter()
        .map(|profile| profile.profile.id.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    let mut claims = BTreeMap::<String, BTreeSet<String>>::new();
    let mut blocked = BTreeSet::<String>::new();

    for profile in profiles.iter() {
        let profile_id = profile.profile.id.as_str();
        for name in names(profile) {
            if name == profile_id {
                continue;
            }
            if profile_ids.contains(name) {
                blocked.insert(name.clone());
            }
            claims
                .entry(name.clone())
                .or_default()
                .insert(profile_id.to_owned());
        }
    }

    for (name, owners) in &claims {
        if owners.len() > 1 {
            blocked.insert(name.clone());
        }
    }

    for name in &blocked {
        let mut owners = claims
            .get(name)
            .map(|owners| owners.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        if profile_ids.contains(name) && !owners.iter().any(|owner| owner == name) {
            owners.push(name.clone());
            owners.sort();
        }
        warnings.push(format!(
            "agent profile {} {:?} is ambiguous across {}; {} disabled",
            kind.label(),
            name,
            owners.join(","),
            kind.label()
        ));
    }

    for profile in profiles.iter_mut() {
        let profile_id = profile.profile.id.as_str().to_owned();
        names_mut(profile).retain(|name| name == &profile_id || !blocked.contains(name));
    }
}

fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

struct BuiltinProfileSpec<'a> {
    id: &'a str,
    kind: AgentProfileKind,
    role: AgentRole,
    description: &'a str,
    instructions: &'a str,
    enabled: bool,
    invocation_policy: AgentInvocationPolicy,
    result_policy: AgentResultPolicy,
    nickname_candidates: &'a [&'a str],
}

fn builtin_profile(
    root_config: &RootConfig,
    spec: BuiltinProfileSpec<'_>,
) -> Result<ResolvedAgentProfile> {
    let role_config = root_config.task.role_config(spec.role);
    let provider = role_config
        .provider
        .clone()
        .unwrap_or_else(|| root_config.agent.provider.clone());
    let model = role_config
        .model
        .clone()
        .unwrap_or_else(|| root_config.agent.model.clone());
    let tool_scope = role_tool_scope(root_config, spec.role);
    let profile = AgentProfile {
        id: AgentProfileId::new(spec.id)?,
        kind: spec.kind,
        description: spec.description.to_owned(),
        instructions: spec.instructions.to_owned(),
        model: Some(model),
        provider: Some(provider),
        reasoning_effort: role_config.reasoning_effort.clone(),
        tool_scope,
        permission_policy: root_config.permission.clone(),
        invocation_policy: spec.invocation_policy,
        result_policy: spec.result_policy,
        user_invocable: spec.invocation_policy.default_user_invocable(),
        model_invocable: spec.invocation_policy.default_model_invocable(),
        skills: Vec::new(),
        mcp_servers: Vec::new(),
        nickname_candidates: spec
            .nickname_candidates
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        aliases: Vec::new(),
        slash_names: Vec::new(),
    };
    Ok(ResolvedAgentProfile {
        source_hash: hash_json(&json!({
            "kind": "builtin_task_role",
            "profile": spec.id,
            "role": spec.role.as_str(),
            "provider": profile.provider.as_deref(),
            "provider_key": provider_config_key(profile.provider.as_deref().unwrap_or_default()),
            "model": profile.model.as_deref(),
            "reasoning_effort": profile.reasoning_effort.as_ref(),
            "tools": &role_config.tools,
            "allow_write_subagents": root_config.task.allow_write_subagents,
        }))?,
        profile,
        enabled: spec.enabled,
        enabled_override: None,
        user_invocable_override: None,
        model_invocable_override: None,
        source: AgentProfileSource::System,
        trust_state: AgentTrustState::Trusted,
    })
}

fn capture_profile_snapshot(profile: &ResolvedAgentProfile) -> Result<AgentProfileSnapshot> {
    let profile_hash = hash_json(&serde_json::to_value(&profile.profile)?)?;
    let tool_hash = hash_json(&serde_json::to_value(&profile.profile.tool_scope)?)?;
    let permission_hash = hash_json(&serde_json::to_value(&profile.profile.permission_policy)?)?;
    let mcp_hash = hash_json(&json!({ "servers": profile.profile.mcp_servers }))?;
    let skill_hashes = profile
        .profile
        .skills
        .iter()
        .map(|skill| hash_json(&json!({ "skill": skill })))
        .collect::<Result<Vec<_>>>()?;
    let snapshot_id = AgentProfileSnapshotId::new(format!(
        "snapshot_{}_{}",
        profile.profile.id.as_str(),
        short_hash(&profile_hash)
    ))?;
    Ok(AgentProfileSnapshot {
        snapshot_id,
        profile_id: profile.profile.id.clone(),
        source: profile.source.clone(),
        source_hash: profile.source_hash.clone(),
        profile_hash,
        resolved_tool_scope_hash: tool_hash,
        resolved_permission_policy_hash: permission_hash,
        resolved_mcp_scope_hash: mcp_hash,
        resolved_skill_hashes: skill_hashes,
        trust_state: profile.trust_state,
    })
}

fn profile_allowed_by_context(
    profile: &ResolvedAgentProfile,
    context: &AgentProfileIndexContext,
) -> bool {
    if let Some(allowed_profile_ids) = &context.allowed_profile_ids
        && !allowed_profile_ids.contains(&profile.profile.id)
    {
        return false;
    }
    tool_scope_contains(&context.tool_scope, &profile.profile.tool_scope)
}

fn tool_scope_contains(parent: &ToolRegistryScope, child: &ToolRegistryScope) -> bool {
    if parent.allow_all {
        return true;
    }
    if child.allow_all {
        return false;
    }
    child.names.iter().all(|name| parent.allows(name))
        && child.prefixes.iter().all(|prefix| parent.allows(prefix))
}

fn model_visible_fingerprint(
    entries: &[ModelVisibleAgentIndexEntry],
    hidden_count: usize,
    context: &AgentProfileIndexContext,
) -> Result<String> {
    let entries_json = entries
        .iter()
        .map(|entry| {
            json!({
                "profile_id": entry.profile_id.as_str(),
                "kind": entry.kind,
                "description": entry.description,
                "result_policy": entry.result_policy,
                "tool_scope_hash": entry.tool_scope_hash,
            })
        })
        .collect::<Vec<_>>();
    hash_json(&json!({
        "entries": entries_json,
        "hidden_count": hidden_count,
        "tool_scope": context.tool_scope,
        "allowed_profile_ids": context.allowed_profile_ids.as_ref().map(|ids| {
            ids.iter().map(|id| id.as_str().to_owned()).collect::<Vec<_>>()
        }),
    }))
}

fn role_tool_scope(root_config: &RootConfig, role: AgentRole) -> ToolRegistryScope {
    let configured = &root_config.task.role_config(role).tools;
    if !configured_allowlist_is_empty(configured) {
        return ToolRegistryScope {
            allow_all: configured.allow_all,
            names: configured.names.iter().cloned().collect(),
            prefixes: configured.prefixes.clone(),
        };
    }
    match role {
        AgentRole::Planner | AgentRole::SubagentRead => read_only_role_tool_scope(),
        AgentRole::Executor => ToolRegistryScope {
            allow_all: true,
            ..ToolRegistryScope::default()
        },
        AgentRole::SubagentWrite if root_config.task.allow_write_subagents => ToolRegistryScope {
            allow_all: true,
            ..ToolRegistryScope::default()
        },
        AgentRole::SubagentWrite => read_only_role_tool_scope(),
    }
}

fn configured_allowlist_is_empty(config: &sigil_kernel::ToolAllowlistConfig) -> bool {
    !config.allow_all && config.names.is_empty() && config.prefixes.is_empty()
}

fn read_only_role_tool_scope() -> ToolRegistryScope {
    ToolRegistryScope::from_names_and_prefixes(
        [
            "read_file",
            "ls",
            "glob",
            "grep",
            "code_symbols",
            "code_workspace_symbols",
            "code_definition",
            "code_references",
            "code_diagnostics",
            LOAD_SKILL_TOOL_NAME,
        ],
        std::iter::empty::<&str>(),
    )
}

fn tool_scope_is_empty(scope: &ToolRegistryScope) -> bool {
    !scope.allow_all && scope.names.is_empty() && scope.prefixes.is_empty()
}

fn hash_json(value: &Value) -> Result<String> {
    let bytes = serde_json::to_vec(value)?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

fn short_hash(hash: &str) -> &str {
    hash.get(..12).unwrap_or(hash)
}

#[cfg(test)]
#[path = "tests/agent_profile_registry_tests.rs"]
mod tests;
