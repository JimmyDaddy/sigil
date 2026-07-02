use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};

use anyhow::{Result, anyhow};
use globset::{Glob, GlobMatcher};
use serde::{Deserialize, Serialize};

use crate::tool::{ToolAccess, ToolSpec, ToolSubject, ToolSubjectKind, ToolSubjectScope};

/// Default interaction surface for one agent run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractionMode {
    Interactive,
    Headless,
}

/// Stable approval modes used by permission policy evaluation.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    Allow,
    #[default]
    Ask,
    Deny,
}

impl ApprovalMode {
    /// Returns the stable config-friendly label.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Ask => "ask",
            Self::Deny => "deny",
        }
    }
}

/// User-facing permission preset. Presets stay intentionally coarse so normal users do not need
/// to understand the internal operation/path-rule lattice.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionPreset {
    ReadOnly,
    #[default]
    Balanced,
}

/// Per-access permission defaults.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PermissionAccessConfig {
    #[serde(default)]
    pub read: Option<ApprovalMode>,
    #[serde(default)]
    pub write: Option<ApprovalMode>,
    #[serde(default)]
    pub execute: Option<ApprovalMode>,
    #[serde(default)]
    pub network: Option<ApprovalMode>,
}

impl Default for PermissionAccessConfig {
    fn default() -> Self {
        Self {
            read: Some(ApprovalMode::Allow),
            write: None,
            execute: None,
            network: None,
        }
    }
}

impl PermissionAccessConfig {
    fn mode_for(&self, access: ToolAccess) -> Option<ApprovalMode> {
        match access {
            ToolAccess::Read => self.read,
            ToolAccess::Write => self.write,
            ToolAccess::Execute => self.execute,
            ToolAccess::Network => self.network,
        }
    }
}

/// One explicit tool permission override rule.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PermissionRule {
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub subject_glob: Option<String>,
    #[serde(default)]
    pub mode: ApprovalMode,
}

/// Advanced guard for explicitly approved paths outside the workspace root.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ExternalDirectoryConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub default_mode: ApprovalMode,
    #[serde(default)]
    pub rules: Vec<ExternalDirectoryRule>,
}

impl Default for ExternalDirectoryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_mode: ApprovalMode::Ask,
            rules: Vec::new(),
        }
    }
}

/// One external-directory permission override.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ExternalDirectoryRule {
    pub path_glob: String,
    #[serde(default)]
    pub mode: ApprovalMode,
}

/// Shared permission policy configuration for one entrypoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PermissionConfig {
    #[serde(default)]
    pub preset: PermissionPreset,
    #[serde(default)]
    pub default_mode: ApprovalMode,
    #[serde(default)]
    pub access: PermissionAccessConfig,
    #[serde(default)]
    pub tools: BTreeMap<String, ApprovalMode>,
    #[serde(default)]
    pub rules: Vec<PermissionRule>,
    #[serde(default)]
    pub external_directory: ExternalDirectoryConfig,
}

impl Default for PermissionConfig {
    fn default() -> Self {
        Self {
            preset: PermissionPreset::default(),
            default_mode: ApprovalMode::Ask,
            access: PermissionAccessConfig::default(),
            tools: BTreeMap::new(),
            rules: Vec::new(),
            external_directory: ExternalDirectoryConfig::default(),
        }
    }
}

/// Provider-neutral operation class derived from a tool call for fine-grained permission logic.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ToolOperation {
    Read,
    Search,
    CreateFile,
    EditFile,
    OverwriteFile,
    DeleteFile,
    RenamePath,
    CreateDirectory,
    DeleteDirectory,
    RecursiveDelete,
    ApplyChangeSet,
    ExecuteReadOnlyCommand,
    ExecuteMutatingCommand,
    ExecuteUnknownCommand,
    ExecuteDestructiveCommand,
    SendTerminalInput,
    NetworkRequest,
    SpawnAgent,
    MessageAgent,
    CloseAgent,
    LoadSkill,
    InvokePlugin,
}

/// Product safety category for one path subject.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PathTrustZone {
    WorkspaceSource,
    WorkspaceDocs,
    WorkspaceProjectAsset,
    WorkspaceRuntimeState,
    WorkspaceIgnored,
    WorkspaceGitMetadata,
    WorkspaceConfigSecret,
    UserState,
    UserCache,
    External,
    Unknown,
}

/// Derived risk label used by policy overlays and approval UI.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum PermissionRisk {
    Low,
    Medium,
    High,
    Destructive,
    Protected,
}

/// Extra confirmation a policy decision may require before the tool can execute.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum PermissionConfirmation {
    Standard,
    TypePath,
    TypePhrase { phrase: String },
}

/// Resolved context supplied by entrypoints once runtime paths and caps are known.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PermissionEvaluationContext {
    pub workspace_root: PathBuf,
    pub project_asset_roots: Vec<PathBuf>,
    pub runtime_state_roots: Vec<PathBuf>,
    pub user_state_roots: Vec<PathBuf>,
    pub user_cache_roots: Vec<PathBuf>,
    pub effective_policy_cap: Option<EffectivePermissionPolicyCap>,
}

/// A materialized permission cap candidate accepted by the decision lattice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectivePermissionPolicyCap {
    pub policy_hash: String,
    pub mode: ApprovalMode,
}

/// One resolved permission decision for a tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionDecision {
    pub mode: ApprovalMode,
    pub access: ToolAccess,
    pub operation: ToolOperation,
    pub risk: PermissionRisk,
    pub subjects: Vec<ToolSubject>,
    pub subject_zones: Vec<PathTrustZone>,
    pub external_directory_required: bool,
    pub confirmation: Option<PermissionConfirmation>,
    pub snapshot_required: bool,
}

impl PermissionDecision {
    /// Constructs a decision and derives operation, path zones, risk, confirmation, and snapshot
    /// metadata from the tool/access/subject tuple.
    pub fn new(
        mode: ApprovalMode,
        tool_name: &str,
        access: ToolAccess,
        subjects: Vec<ToolSubject>,
        external_directory_required: bool,
    ) -> Self {
        Self::new_with_operation(
            mode,
            infer_tool_operation(tool_name, access),
            access,
            subjects,
            external_directory_required,
        )
    }

    /// Constructs a decision with a caller-provided fine-grained operation.
    pub fn new_with_operation(
        mode: ApprovalMode,
        operation: ToolOperation,
        access: ToolAccess,
        subjects: Vec<ToolSubject>,
        external_directory_required: bool,
    ) -> Self {
        let subject_zones = subjects
            .iter()
            .map(classify_path_trust_zone)
            .collect::<Vec<_>>();
        Self::new_with_operation_and_zones(
            mode,
            operation,
            access,
            subjects,
            subject_zones,
            external_directory_required,
        )
    }

    /// Constructs a decision with pre-classified path zones from the active runtime context.
    pub fn new_with_operation_and_zones(
        mode: ApprovalMode,
        operation: ToolOperation,
        access: ToolAccess,
        subjects: Vec<ToolSubject>,
        subject_zones: Vec<PathTrustZone>,
        external_directory_required: bool,
    ) -> Self {
        let risk = derive_permission_risk(access, operation, &subject_zones);
        let mode = apply_operation_risk_overlay(mode, operation, risk);
        let confirmation = confirmation_for_risk(risk, &subject_zones).or_else(|| {
            (access == ToolAccess::Write && subject_zones.contains(&PathTrustZone::External))
                .then_some(PermissionConfirmation::TypePath)
        });
        let snapshot_required = matches!(risk, PermissionRisk::Destructive);
        Self {
            mode,
            access,
            operation,
            risk,
            subjects,
            subject_zones,
            external_directory_required,
            confirmation,
            snapshot_required,
        }
    }
}

/// Policy evaluator that resolves allow/ask/deny for one tool call.
pub struct PermissionPolicy<'a> {
    config: &'a PermissionConfig,
    context: Option<&'a PermissionEvaluationContext>,
    rules: Vec<CompiledPermissionRule<'a>>,
    external_rules: Vec<CompiledExternalDirectoryRule<'a>>,
}

impl<'a> PermissionPolicy<'a> {
    /// Creates a policy evaluator from shared configuration.
    pub fn new(config: &'a PermissionConfig) -> Self {
        Self {
            config,
            context: None,
            rules: config
                .rules
                .iter()
                .map(CompiledPermissionRule::new)
                .collect(),
            external_rules: config
                .external_directory
                .rules
                .iter()
                .map(CompiledExternalDirectoryRule::new)
                .collect(),
        }
    }

    /// Creates a policy evaluator with the resolved runtime path context.
    pub fn new_with_context(
        config: &'a PermissionConfig,
        context: &'a PermissionEvaluationContext,
    ) -> Self {
        Self {
            config,
            context: Some(context),
            rules: config
                .rules
                .iter()
                .map(CompiledPermissionRule::new)
                .collect(),
            external_rules: config
                .external_directory
                .rules
                .iter()
                .map(CompiledExternalDirectoryRule::new)
                .collect(),
        }
    }

    /// Resolves one tool call decision from the tool spec, stable name, and subjects.
    ///
    /// # Errors
    ///
    /// Returns an error when one configured subject glob is invalid.
    pub fn decide(
        &self,
        spec: &ToolSpec,
        tool_name: &str,
        subjects: Vec<ToolSubject>,
    ) -> Result<PermissionDecision> {
        self.decide_with_access(spec, tool_name, spec.access, subjects)
    }

    /// Resolves one tool call decision using a dynamic access class derived from call arguments.
    ///
    /// # Errors
    ///
    /// Returns an error when one configured subject glob is invalid.
    pub fn decide_with_access(
        &self,
        spec: &ToolSpec,
        tool_name: &str,
        access: ToolAccess,
        subjects: Vec<ToolSubject>,
    ) -> Result<PermissionDecision> {
        self.decide_with_access_and_default(spec, tool_name, access, subjects, None)
    }

    /// Resolves one tool call decision with a tool-provided default approval mode.
    ///
    /// # Errors
    ///
    /// Returns an error when one configured subject glob is invalid.
    pub fn decide_with_access_and_default(
        &self,
        spec: &ToolSpec,
        tool_name: &str,
        access: ToolAccess,
        subjects: Vec<ToolSubject>,
        tool_default_mode: Option<ApprovalMode>,
    ) -> Result<PermissionDecision> {
        self.decide_with_operation_and_default(
            spec,
            tool_name,
            access,
            infer_tool_operation(tool_name, access),
            subjects,
            tool_default_mode,
        )
    }

    /// Resolves one tool call decision with a tool-provided operation and default approval mode.
    ///
    /// # Errors
    ///
    /// Returns an error when one configured subject glob is invalid.
    pub fn decide_with_operation_and_default(
        &self,
        _spec: &ToolSpec,
        tool_name: &str,
        access: ToolAccess,
        operation: ToolOperation,
        subjects: Vec<ToolSubject>,
        tool_default_mode: Option<ApprovalMode>,
    ) -> Result<PermissionDecision> {
        let external_directory_required = subjects
            .iter()
            .any(|subject| subject.scope == ToolSubjectScope::External)
            && !self.config.external_directory.enabled;
        let subject_modes = if subjects.is_empty() {
            vec![self.decide_one_subject(tool_name, access, tool_default_mode, None)?]
        } else {
            subjects
                .iter()
                .map(|subject| {
                    self.decide_one_subject(tool_name, access, tool_default_mode, Some(subject))
                })
                .collect::<Result<Vec<_>>>()?
        };

        let mut mode = combine_modes(subject_modes);
        if let Some(cap_mode) = self
            .context
            .and_then(|context| context.effective_policy_cap.as_ref())
            .map(|cap| cap.mode)
        {
            mode = combine_modes(vec![mode, cap_mode]);
        }
        let subject_zones = self.classify_subject_zones(&subjects);

        Ok(PermissionDecision::new_with_operation_and_zones(
            mode,
            operation,
            access,
            subjects,
            subject_zones,
            external_directory_required,
        ))
    }

    fn classify_subject_zones(&self, subjects: &[ToolSubject]) -> Vec<PathTrustZone> {
        subjects
            .iter()
            .map(|subject| {
                self.context.map_or_else(
                    || classify_path_trust_zone(subject),
                    |context| classify_path_trust_zone_with_context(subject, context),
                )
            })
            .collect()
    }

    fn decide_one_subject(
        &self,
        tool_name: &str,
        access: ToolAccess,
        tool_default_mode: Option<ApprovalMode>,
        subject: Option<&ToolSubject>,
    ) -> Result<ApprovalMode> {
        if self.config.preset == PermissionPreset::ReadOnly && access != ToolAccess::Read {
            return Ok(ApprovalMode::Deny);
        }

        let mut mode = self
            .config
            .access
            .mode_for(access)
            .unwrap_or(self.config.default_mode);
        if let Some(tool_default_mode) = tool_default_mode {
            mode = tool_default_mode;
        }
        if let Some(tool_mode) = self.config.tools.get(tool_name).copied() {
            mode = tool_mode;
        }

        let matching_rule_modes = self
            .rules
            .iter()
            .filter(|compiled| {
                compiled
                    .rule
                    .tool_name
                    .as_deref()
                    .is_none_or(|configured| configured == tool_name)
            })
            .filter_map(
                |compiled| match compiled.matches_subject(tool_name, subject) {
                    Ok(true) => Some(Ok(compiled.rule.mode)),
                    Ok(false) => None,
                    Err(error) => Some(Err(error)),
                },
            )
            .collect::<Result<Vec<_>>>()?;

        let tool_policy_mode = if matching_rule_modes.is_empty() {
            mode
        } else {
            combine_modes(matching_rule_modes)
        };

        let Some(subject) = subject else {
            return Ok(tool_policy_mode);
        };
        if subject.scope == ToolSubjectScope::External {
            Ok(combine_modes(vec![
                tool_policy_mode,
                self.decide_external_subject(subject)?,
            ]))
        } else {
            Ok(tool_policy_mode)
        }
    }

    fn decide_external_subject(&self, subject: &ToolSubject) -> Result<ApprovalMode> {
        let config = &self.config.external_directory;
        if !config.enabled {
            return Ok(ApprovalMode::Deny);
        }

        let matching_rule_modes = self
            .external_rules
            .iter()
            .filter_map(|compiled| match compiled.matches_subject(subject) {
                Ok(true) => Some(Ok(compiled.rule.mode)),
                Ok(false) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<Result<Vec<_>>>()?;

        if matching_rule_modes.is_empty() {
            Ok(config.default_mode)
        } else {
            Ok(combine_modes(matching_rule_modes))
        }
    }
}

/// Returns the best currently-known operation classification for one tool.
pub fn infer_tool_operation(tool_name: &str, access: ToolAccess) -> ToolOperation {
    match tool_name {
        "read_file" => ToolOperation::Read,
        "ls" | "glob" | "grep" => ToolOperation::Search,
        "write_file" => ToolOperation::OverwriteFile,
        "edit_file" => ToolOperation::EditFile,
        "delete_file" => ToolOperation::DeleteFile,
        "apply_changeset" => ToolOperation::ApplyChangeSet,
        "terminal_input" => ToolOperation::SendTerminalInput,
        "spawn_agent" => ToolOperation::SpawnAgent,
        "message_agent" => ToolOperation::MessageAgent,
        "close_agent" => ToolOperation::CloseAgent,
        "load_skill" => ToolOperation::LoadSkill,
        "bash" | "terminal_start" if access == ToolAccess::Read => {
            ToolOperation::ExecuteReadOnlyCommand
        }
        "bash" | "terminal_start" => ToolOperation::ExecuteUnknownCommand,
        _ => match access {
            ToolAccess::Read => ToolOperation::Read,
            ToolAccess::Write => ToolOperation::EditFile,
            ToolAccess::Execute => ToolOperation::ExecuteUnknownCommand,
            ToolAccess::Network => ToolOperation::NetworkRequest,
        },
    }
}

/// Classifies a path subject into a trust zone using conservative built-in defaults.
pub fn classify_path_trust_zone(subject: &ToolSubject) -> PathTrustZone {
    if subject.scope == ToolSubjectScope::External {
        return PathTrustZone::External;
    }
    if subject.kind != ToolSubjectKind::Path {
        return PathTrustZone::Unknown;
    }

    let normalized = subject.normalized.trim_start_matches("./");
    if path_is_under(normalized, ".git") {
        return PathTrustZone::WorkspaceGitMetadata;
    }
    if normalized == ".sigil" {
        return PathTrustZone::WorkspaceRuntimeState;
    }
    if path_is_under_any(
        normalized,
        &[
            ".sigil/sessions",
            ".sigil/state",
            ".sigil/cache",
            ".sigil/tasks",
            ".sigil/changesets",
            ".sigil/tmp",
        ],
    ) {
        return PathTrustZone::WorkspaceRuntimeState;
    }
    if path_is_under_any(
        normalized,
        &[".sigil/agents", ".sigil/skills", ".sigil/plugins"],
    ) {
        return PathTrustZone::WorkspaceProjectAsset;
    }
    if normalized == "sigil.toml"
        || normalized == ".env"
        || normalized.starts_with(".env.")
        || normalized.contains("credentials")
        || normalized.contains("secret")
    {
        return PathTrustZone::WorkspaceConfigSecret;
    }
    if normalized.starts_with("docs/") || normalized.starts_with("dev/docs/") {
        return PathTrustZone::WorkspaceDocs;
    }
    PathTrustZone::WorkspaceSource
}

/// Classifies a path subject into a trust zone using the active runtime path context.
pub fn classify_path_trust_zone_with_context(
    subject: &ToolSubject,
    context: &PermissionEvaluationContext,
) -> PathTrustZone {
    if subject.scope == ToolSubjectScope::External {
        return PathTrustZone::External;
    }
    if subject.kind != ToolSubjectKind::Path {
        return PathTrustZone::Unknown;
    }

    let Some(subject_path) = subject_path_for_context(subject, context) else {
        return classify_path_trust_zone(subject);
    };
    if path_starts_with_any_context_root(&subject_path, &context.runtime_state_roots, context) {
        return PathTrustZone::WorkspaceRuntimeState;
    }
    if path_starts_with_any_context_root(&subject_path, &context.project_asset_roots, context) {
        return PathTrustZone::WorkspaceProjectAsset;
    }
    if path_starts_with_any_context_root(&subject_path, &context.user_state_roots, context) {
        return PathTrustZone::UserState;
    }
    if path_starts_with_any_context_root(&subject_path, &context.user_cache_roots, context) {
        return PathTrustZone::UserCache;
    }

    let workspace_root = normalize_policy_path(&context.workspace_root);
    if !workspace_root.as_os_str().is_empty() && !subject_path.starts_with(&workspace_root) {
        return PathTrustZone::External;
    }

    classify_path_trust_zone(subject)
}

fn subject_path_for_context(
    subject: &ToolSubject,
    context: &PermissionEvaluationContext,
) -> Option<PathBuf> {
    subject
        .canonical_path
        .as_ref()
        .cloned()
        .or_else(|| {
            let normalized = subject.normalized.trim();
            if normalized.is_empty() {
                return None;
            }
            let path = PathBuf::from(normalized);
            Some(if path.is_absolute() {
                path
            } else {
                context.workspace_root.join(path)
            })
        })
        .map(|path| normalize_policy_path(&path))
}

fn path_starts_with_any_context_root(
    path: &Path,
    roots: &[PathBuf],
    context: &PermissionEvaluationContext,
) -> bool {
    roots.iter().any(|root| {
        let root = if root.is_absolute() {
            root.clone()
        } else {
            context.workspace_root.join(root)
        };
        let root = normalize_policy_path(&root);
        !root.as_os_str().is_empty() && path.starts_with(root)
    })
}

fn normalize_policy_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

/// Derives a conservative risk label from access, operation, and path zones.
pub fn derive_permission_risk(
    access: ToolAccess,
    operation: ToolOperation,
    zones: &[PathTrustZone],
) -> PermissionRisk {
    if zones.iter().any(|zone| {
        matches!(
            zone,
            PathTrustZone::WorkspaceGitMetadata
                | PathTrustZone::WorkspaceRuntimeState
                | PathTrustZone::WorkspaceConfigSecret
                | PathTrustZone::UserState
                | PathTrustZone::UserCache
        )
    }) && access != ToolAccess::Read
    {
        return PermissionRisk::Protected;
    }

    if matches!(
        operation,
        ToolOperation::DeleteFile
            | ToolOperation::DeleteDirectory
            | ToolOperation::RecursiveDelete
            | ToolOperation::ApplyChangeSet
            | ToolOperation::ExecuteDestructiveCommand
    ) {
        return PermissionRisk::Destructive;
    }

    if matches!(
        operation,
        ToolOperation::ExecuteUnknownCommand | ToolOperation::SendTerminalInput
    ) {
        return PermissionRisk::High;
    }

    match access {
        ToolAccess::Read => PermissionRisk::Low,
        ToolAccess::Write => PermissionRisk::Medium,
        ToolAccess::Execute | ToolAccess::Network => PermissionRisk::High,
    }
}

/// Applies safety overlays that no allow source can bypass.
pub fn apply_risk_overlay(mode: ApprovalMode, risk: PermissionRisk) -> ApprovalMode {
    apply_operation_risk_overlay(mode, ToolOperation::Read, risk)
}

/// Returns true when an interactive approval can safely be widened to a session-local grant.
pub fn tool_approval_session_grant_available(decision: &PermissionDecision) -> bool {
    tool_approval_session_grant_available_for_parts(
        decision.access,
        decision.operation,
        decision.risk,
        &decision.subjects,
        &decision.subject_zones,
        decision.confirmation.as_ref(),
        decision.snapshot_required,
    )
}

/// Returns true when the supplied approval metadata can safely be widened to a session-local
/// grant. This helper is shared by the kernel executor and TUI so the UI does not advertise
/// a grant action that the executor would reject.
pub fn tool_approval_session_grant_available_for_parts(
    access: ToolAccess,
    operation: ToolOperation,
    risk: PermissionRisk,
    subjects: &[ToolSubject],
    zones: &[PathTrustZone],
    confirmation: Option<&PermissionConfirmation>,
    snapshot_required: bool,
) -> bool {
    if confirmation.is_some() || snapshot_required || subjects.is_empty() {
        return false;
    }
    let exact_command_grant_available =
        exact_command_session_grant_available(access, operation, risk, subjects, zones);
    if !matches!(risk, PermissionRisk::Low | PermissionRisk::Medium)
        && !exact_command_grant_available
    {
        return false;
    }
    if zones.contains(&PathTrustZone::External) && access != ToolAccess::Read {
        return false;
    }
    if !matches!(
        operation,
        ToolOperation::Read
            | ToolOperation::Search
            | ToolOperation::CreateFile
            | ToolOperation::EditFile
            | ToolOperation::OverwriteFile
            | ToolOperation::CreateDirectory
            | ToolOperation::ExecuteReadOnlyCommand
            | ToolOperation::ExecuteUnknownCommand
    ) {
        return false;
    }
    subjects.iter().all(|subject| {
        subject_has_stable_session_grant_scope(subject, operation, exact_command_grant_available)
    })
}

fn exact_command_session_grant_available(
    access: ToolAccess,
    operation: ToolOperation,
    risk: PermissionRisk,
    subjects: &[ToolSubject],
    zones: &[PathTrustZone],
) -> bool {
    let mut command_subjects = subjects
        .iter()
        .filter(|subject| subject.kind == ToolSubjectKind::Command);
    access == ToolAccess::Execute
        && operation == ToolOperation::ExecuteUnknownCommand
        && risk == PermissionRisk::High
        && zones
            .iter()
            .all(|zone| !matches!(zone, PathTrustZone::External))
        && command_subjects
            .next()
            .is_some_and(command_subject_is_stable_and_exact)
        && command_subjects.all(command_subject_is_stable_and_exact)
}

fn command_subject_is_stable_and_exact(subject: &ToolSubject) -> bool {
    if subject.kind != ToolSubjectKind::Command {
        return false;
    }
    let normalized = subject.normalized.trim();
    !normalized.is_empty()
        && normalized
            == subject
                .original
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
}

fn subject_has_stable_session_grant_scope(
    subject: &ToolSubject,
    operation: ToolOperation,
    exact_command_grant_available: bool,
) -> bool {
    match subject.kind {
        ToolSubjectKind::Path => match subject.scope {
            ToolSubjectScope::Workspace => !subject.normalized.trim().is_empty(),
            ToolSubjectScope::External => subject
                .canonical_path
                .as_ref()
                .is_some_and(|path| !path.as_os_str().is_empty()),
            ToolSubjectScope::Unknown => false,
        },
        ToolSubjectKind::Command => {
            (operation == ToolOperation::ExecuteReadOnlyCommand || exact_command_grant_available)
                && !subject.normalized.trim().is_empty()
        }
        _ => false,
    }
}

fn apply_operation_risk_overlay(
    mode: ApprovalMode,
    operation: ToolOperation,
    risk: PermissionRisk,
) -> ApprovalMode {
    match risk {
        PermissionRisk::Protected => ApprovalMode::Deny,
        PermissionRisk::Destructive if mode == ApprovalMode::Allow => ApprovalMode::Ask,
        PermissionRisk::High if operation == ToolOperation::SendTerminalInput => {
            combine_modes(vec![mode, ApprovalMode::Ask])
        }
        _ => mode,
    }
}

fn confirmation_for_risk(
    risk: PermissionRisk,
    zones: &[PathTrustZone],
) -> Option<PermissionConfirmation> {
    if risk == PermissionRisk::Destructive
        && zones
            .iter()
            .any(|zone| matches!(zone, PathTrustZone::WorkspaceProjectAsset))
    {
        return Some(PermissionConfirmation::TypePath);
    }
    None
}

fn path_is_under_any(path: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| path_is_under(path, prefix))
}

fn path_is_under(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

struct CompiledPermissionRule<'a> {
    rule: &'a PermissionRule,
    subject_matcher: CompiledMatcher,
}

impl<'a> CompiledPermissionRule<'a> {
    fn new(rule: &'a PermissionRule) -> Self {
        let subject_matcher = match rule.subject_glob.as_deref() {
            Some(subject_glob) => compile_permission_glob(subject_glob),
            None => CompiledMatcher::Any,
        };
        Self {
            rule,
            subject_matcher,
        }
    }

    fn matches_subject(&self, tool_name: &str, subject: Option<&ToolSubject>) -> Result<bool> {
        let CompiledMatcher::Any = &self.subject_matcher else {
            let subject_ref = subject
                .map(|subject| subject.normalized.as_str())
                .ok_or_else(|| anyhow!("permission rule requires a subject for {tool_name}"))?;
            return self.subject_matcher.is_match(subject_ref);
        };
        Ok(true)
    }
}

struct CompiledExternalDirectoryRule<'a> {
    rule: &'a ExternalDirectoryRule,
    matcher: CompiledMatcher,
}

impl<'a> CompiledExternalDirectoryRule<'a> {
    fn new(rule: &'a ExternalDirectoryRule) -> Self {
        let matcher = canonical_external_rule_pattern(&rule.path_glob)
            .and_then(|pattern| compile_external_glob(&rule.path_glob, &pattern))
            .map_or_else(
                |error| CompiledMatcher::Invalid(error.to_string()),
                CompiledMatcher::Glob,
            );
        Self { rule, matcher }
    }

    fn matches_subject(&self, subject: &ToolSubject) -> Result<bool> {
        let Some(canonical_path) = subject.canonical_path.as_ref() else {
            return Ok(false);
        };
        self.matcher.is_match(canonical_path)
    }
}

enum CompiledMatcher {
    Any,
    Glob(GlobMatcher),
    Invalid(String),
}

impl CompiledMatcher {
    fn is_match(&self, value: impl AsRef<Path>) -> Result<bool> {
        match self {
            Self::Any => Ok(true),
            Self::Glob(matcher) => Ok(matcher.is_match(value)),
            Self::Invalid(message) => Err(anyhow!("{message}")),
        }
    }
}

fn compile_permission_glob(subject_glob: &str) -> CompiledMatcher {
    Glob::new(subject_glob).map_or_else(
        |error| {
            CompiledMatcher::Invalid(format!("invalid permission glob {subject_glob}: {error}"))
        },
        |glob| CompiledMatcher::Glob(glob.compile_matcher()),
    )
}

fn compile_external_glob(path_glob: &str, pattern: &str) -> Result<GlobMatcher> {
    Glob::new(pattern)
        .map_err(|error| anyhow!("invalid external directory glob {path_glob}: {error}"))
        .map(|glob| glob.compile_matcher())
}

fn canonical_external_rule_pattern(path_glob: &str) -> Result<String> {
    let expanded = expand_external_rule_path(path_glob)?;
    reject_parent_components(&expanded, "external directory path_glob")?;
    let expanded_path = Path::new(&expanded);
    if !expanded_path.is_absolute() {
        return Err(anyhow!(
            "external directory path_glob must be absolute, ~/..., or $HOME/..."
        ));
    }

    let mut literal_prefix = PathBuf::new();
    let mut glob_suffix = PathBuf::new();
    let mut in_glob_suffix = false;
    for component in expanded_path.components() {
        let part = component.as_os_str().to_string_lossy();
        if !in_glob_suffix && !contains_glob_token(&part) {
            literal_prefix.push(component.as_os_str());
        } else {
            in_glob_suffix = true;
            glob_suffix.push(component.as_os_str());
        }
    }

    if literal_prefix.as_os_str().is_empty() {
        literal_prefix.push(Path::new("/"));
    }
    let canonical_prefix = std::fs::canonicalize(&literal_prefix).map_err(|error| {
        anyhow!(
            "external directory literal prefix {} is not available: {error}",
            literal_prefix.display()
        )
    })?;
    let pattern = if glob_suffix.as_os_str().is_empty() {
        canonical_prefix
    } else {
        canonical_prefix.join(glob_suffix)
    };
    Ok(pattern.to_string_lossy().to_string())
}

fn expand_external_rule_path(path_glob: &str) -> Result<String> {
    let expanded = if path_glob == "~" {
        home_dir()?.to_string_lossy().to_string()
    } else if let Some(rest) = path_glob.strip_prefix("~/") {
        home_dir()?.join(rest).to_string_lossy().to_string()
    } else if path_glob == "$HOME" {
        home_dir()?.to_string_lossy().to_string()
    } else if let Some(rest) = path_glob.strip_prefix("$HOME/") {
        home_dir()?.join(rest).to_string_lossy().to_string()
    } else {
        path_glob.to_owned()
    };
    if expanded.contains('$') {
        return Err(anyhow!(
            "external directory path_glob only supports $HOME expansion"
        ));
    }
    Ok(expanded)
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME is not set for external directory path expansion"))
}

fn reject_parent_components(path: &str, label: &str) -> Result<()> {
    if Path::new(path)
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(anyhow!("{label} must not contain .. components"));
    }
    Ok(())
}

fn contains_glob_token(value: &str) -> bool {
    value.contains('*') || value.contains('?') || value.contains('[')
}

pub fn combine_modes(modes: Vec<ApprovalMode>) -> ApprovalMode {
    if modes.iter().any(|mode| matches!(mode, ApprovalMode::Deny)) {
        ApprovalMode::Deny
    } else if modes.iter().any(|mode| matches!(mode, ApprovalMode::Ask)) {
        ApprovalMode::Ask
    } else {
        ApprovalMode::Allow
    }
}

#[cfg(test)]
#[path = "tests/permission_tests.rs"]
mod tests;
