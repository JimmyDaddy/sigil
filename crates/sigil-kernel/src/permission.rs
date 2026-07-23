use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};

use anyhow::{Result, anyhow};
use globset::{Glob, GlobMatcher};
use serde::{Deserialize, Serialize, de};

use crate::tool::{
    NetworkEffect, ToolAccess, ToolSpec, ToolSubject, ToolSubjectKind, ToolSubjectScope,
};

const WORKSPACE_RUNTIME_STATE_PATHS: &[&str] = &[
    ".sigil/sessions",
    ".sigil/state",
    ".sigil/cache",
    ".sigil/tasks",
    ".sigil/changesets",
    ".sigil/tmp",
];
const WORKSPACE_PROJECT_ASSET_PATHS: &[&str] =
    &[".sigil/agents", ".sigil/skills", ".sigil/plugins"];
const WORKSPACE_DOC_PATHS: &[&str] = &["docs", "dev/docs"];

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

/// Permission facets that one durable session-local tool grant may relax.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolApprovalSessionGrantFacet {
    Local,
    Network,
}

impl ToolApprovalSessionGrantFacet {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Network => "network",
        }
    }
}

/// Subject matching scope for one durable session-local tool grant.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolApprovalSessionGrantScope {
    #[default]
    ExactSubjects,
    /// Same tool and read-only network operation; destination controls still run per call.
    NetworkReadTool,
}

impl ToolApprovalSessionGrantScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExactSubjects => "exact_subjects",
            Self::NetworkReadTool => "network_read_tool",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolApprovalSessionGrantShape {
    pub(crate) facets: Vec<ToolApprovalSessionGrantFacet>,
    pub(crate) scope: ToolApprovalSessionGrantScope,
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

/// Independent runtime policy for declared or dynamically resolved network effects.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicy {
    #[default]
    Allow,
    Ask,
    Deny,
}

impl NetworkPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Ask => "ask",
            Self::Deny => "deny",
        }
    }

    fn approval_mode(self) -> ApprovalMode {
        match self {
            Self::Allow => ApprovalMode::Allow,
            Self::Ask => ApprovalMode::Ask,
            Self::Deny => ApprovalMode::Deny,
        }
    }
}

/// User-facing permission mode for one agent run.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionMode {
    ReadOnly,
    #[default]
    Manual,
    AutoEdit,
    DangerFullAccess,
}

impl PermissionMode {
    /// Returns the stable config-friendly label.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::Manual => "manual",
            Self::AutoEdit => "auto-edit",
            Self::DangerFullAccess => "danger-full-access",
        }
    }

    /// Returns a conservative access-level baseline when no concrete subject is available.
    pub fn baseline_for_access(self, access: ToolAccess) -> ApprovalMode {
        let operation = match access {
            ToolAccess::Read => ToolOperation::Read,
            ToolAccess::Write => ToolOperation::EditFile,
            ToolAccess::Execute => ToolOperation::ExecuteUnknownCommand,
        };
        self.baseline_for(access, operation, None)
    }

    fn baseline_for(
        self,
        access: ToolAccess,
        operation: ToolOperation,
        zone: Option<PathTrustZone>,
    ) -> ApprovalMode {
        match self {
            Self::ReadOnly => {
                if access == ToolAccess::Read {
                    ApprovalMode::Allow
                } else {
                    ApprovalMode::Deny
                }
            }
            Self::Manual => match access {
                ToolAccess::Read => ApprovalMode::Allow,
                ToolAccess::Write | ToolAccess::Execute => ApprovalMode::Ask,
            },
            Self::AutoEdit => auto_edit_baseline(access, operation, zone),
            Self::DangerFullAccess => ApprovalMode::Allow,
        }
    }
}

fn auto_edit_baseline(
    access: ToolAccess,
    operation: ToolOperation,
    zone: Option<PathTrustZone>,
) -> ApprovalMode {
    match access {
        ToolAccess::Read => ApprovalMode::Allow,
        ToolAccess::Execute => ApprovalMode::Ask,
        ToolAccess::Write => {
            if auto_edit_write_operation_allowed(operation)
                && zone.is_some_and(auto_edit_workspace_zone_allowed)
            {
                ApprovalMode::Allow
            } else {
                ApprovalMode::Ask
            }
        }
    }
}

fn auto_edit_write_operation_allowed(operation: ToolOperation) -> bool {
    matches!(
        operation,
        ToolOperation::CreateFile
            | ToolOperation::EditFile
            | ToolOperation::OverwriteFile
            | ToolOperation::CreateDirectory
    )
}

fn auto_edit_workspace_zone_allowed(zone: PathTrustZone) -> bool {
    matches!(
        zone,
        PathTrustZone::WorkspaceSource
            | PathTrustZone::WorkspaceDocs
            | PathTrustZone::WorkspaceProjectAsset
            | PathTrustZone::WorkspaceIgnored
    )
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

/// User-configured command permission patterns grouped by action.
///
/// Patterns are matched against the normalized command text using only `*` and `?` wildcards.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CommandPermissionConfig {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub ask: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

impl CommandPermissionConfig {
    /// Returns the total number of configured command patterns.
    pub fn pattern_count(&self) -> usize {
        self.allow.len() + self.ask.len() + self.deny.len()
    }

    /// Appends command patterns from another config and validates exact cross-group duplicates.
    pub fn extend_from(&mut self, other: &Self) -> Result<()> {
        self.allow
            .extend(normalize_command_permission_patterns(other.allow.clone()));
        self.ask
            .extend(normalize_command_permission_patterns(other.ask.clone()));
        self.deny
            .extend(normalize_command_permission_patterns(other.deny.clone()));
        validate_command_permission_config(self).map_err(|message| anyhow!("{message}"))
    }
}

impl<'de> Deserialize<'de> for CommandPermissionConfig {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case", deny_unknown_fields)]
        struct RawCommandPermissionConfig {
            #[serde(default)]
            allow: Vec<String>,
            #[serde(default)]
            ask: Vec<String>,
            #[serde(default)]
            deny: Vec<String>,
        }

        let raw = RawCommandPermissionConfig::deserialize(deserializer)?;
        let config = Self {
            allow: normalize_command_permission_patterns(raw.allow),
            ask: normalize_command_permission_patterns(raw.ask),
            deny: normalize_command_permission_patterns(raw.deny),
        };
        validate_command_permission_config(&config).map_err(de::Error::custom)?;
        Ok(config)
    }
}

/// Stable command permission groups for summaries and diagnostics.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommandPermissionGroup {
    Allow,
    Ask,
    Deny,
}

impl CommandPermissionGroup {
    /// Returns the stable config label for this group.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Ask => "ask",
            Self::Deny => "deny",
        }
    }

    fn action(self) -> ApprovalMode {
        match self {
            Self::Allow => ApprovalMode::Allow,
            Self::Ask => ApprovalMode::Ask,
            Self::Deny => ApprovalMode::Deny,
        }
    }
}

/// One command permission pattern that matched a shell-like tool call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CommandPermissionMatch {
    pub group: CommandPermissionGroup,
    pub pattern: String,
    pub command: String,
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
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PermissionConfig {
    #[serde(default)]
    pub mode: PermissionMode,
    #[serde(default)]
    pub commands: CommandPermissionConfig,
    #[serde(default)]
    pub tools: BTreeMap<String, ApprovalMode>,
    #[serde(default)]
    pub rules: Vec<PermissionRule>,
    #[serde(default)]
    pub external_directory: ExternalDirectoryConfig,
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
    ExecuteWorkspaceCheckCommand,
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

/// Additional risk signals that can apply independently from a path's primary trust zone.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PathRiskOverlay {
    SensitiveName,
}

/// Product safety classification for one path subject.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PathTrustAnalysis {
    pub zone: PathTrustZone,
    pub overlays: Vec<PathRiskOverlay>,
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
    /// Additional materialized delegated policies that may only narrow the primary run policy.
    ///
    /// Child-agent runs use this for role and profile constraints. Every layer is evaluated
    /// against the same concrete tool call and subjects before the decisions are combined with
    /// `Deny > Ask > Allow`; callers must not pre-merge these configs by overwriting fields. Each
    /// entry must be a complete policy derived from its parent, not a sparse config whose serde
    /// defaults would accidentally introduce new restrictions.
    pub delegated_policy_constraints: Vec<PermissionConfig>,
    pub effective_policy_cap: Option<EffectivePermissionPolicyCap>,
    pub network_policy: NetworkPolicy,
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
    pub network_effect: Option<NetworkEffect>,
    pub local_policy_decision: ApprovalMode,
    pub network_policy_decision: ApprovalMode,
    pub source_policy_decision: ApprovalMode,
    pub operation: ToolOperation,
    pub risk: PermissionRisk,
    pub subjects: Vec<ToolSubject>,
    pub subject_zones: Vec<PathTrustZone>,
    pub subject_risk_overlays: Vec<PathRiskOverlay>,
    pub external_directory_required: bool,
    pub confirmation: Option<PermissionConfirmation>,
    pub snapshot_required: bool,
    pub command_permission_matches: Vec<CommandPermissionMatch>,
    base_local_policy_decision: ApprovalMode,
    external_directory_policy_decision: ApprovalMode,
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
        let subject_analyses = subjects
            .iter()
            .map(classify_path_trust_analysis)
            .collect::<Vec<_>>();
        let subject_zones = subject_analyses
            .iter()
            .map(|analysis| analysis.zone)
            .collect::<Vec<_>>();
        let subject_risk_overlays = collect_path_risk_overlays(&subject_analyses);
        Self::new_with_operation_zones_and_overlays(
            mode,
            operation,
            access,
            subjects,
            subject_zones,
            subject_risk_overlays,
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
        let subject_risk_overlays = subjects
            .iter()
            .flat_map(path_risk_overlays)
            .collect::<Vec<_>>();
        Self::new_with_operation_zones_and_overlays(
            mode,
            operation,
            access,
            subjects,
            subject_zones,
            subject_risk_overlays,
            external_directory_required,
        )
    }

    /// Constructs a decision with pre-classified path zones and risk overlays.
    pub fn new_with_operation_zones_and_overlays(
        mode: ApprovalMode,
        operation: ToolOperation,
        access: ToolAccess,
        subjects: Vec<ToolSubject>,
        subject_zones: Vec<PathTrustZone>,
        subject_risk_overlays: Vec<PathRiskOverlay>,
        external_directory_required: bool,
    ) -> Self {
        Self::new_with_policy_mode_operation_zones_and_overlays(
            PermissionMode::Manual,
            mode,
            operation,
            access,
            subjects,
            subject_zones,
            subject_risk_overlays,
            external_directory_required,
        )
    }

    /// Constructs a decision from an active user-facing permission mode.
    pub fn new_with_policy_mode_operation_zones_and_overlays(
        policy_mode: PermissionMode,
        mode: ApprovalMode,
        operation: ToolOperation,
        access: ToolAccess,
        subjects: Vec<ToolSubject>,
        subject_zones: Vec<PathTrustZone>,
        subject_risk_overlays: Vec<PathRiskOverlay>,
        external_directory_required: bool,
    ) -> Self {
        Self::new_with_policy_facets_operation_zones_and_overlays(
            policy_mode,
            mode,
            ApprovalMode::Allow,
            ApprovalMode::Allow,
            ApprovalMode::Allow,
            None,
            operation,
            access,
            subjects,
            subject_zones,
            subject_risk_overlays,
            external_directory_required,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_with_policy_facets_operation_zones_and_overlays(
        policy_mode: PermissionMode,
        local_policy_decision: ApprovalMode,
        network_policy_decision: ApprovalMode,
        delegated_source_policy_decision: ApprovalMode,
        external_directory_policy_decision: ApprovalMode,
        network_effect: Option<NetworkEffect>,
        operation: ToolOperation,
        access: ToolAccess,
        subjects: Vec<ToolSubject>,
        subject_zones: Vec<PathTrustZone>,
        subject_risk_overlays: Vec<PathRiskOverlay>,
        external_directory_required: bool,
    ) -> Self {
        let risk = derive_permission_risk_with_network_effect(
            access,
            network_effect,
            operation,
            &subject_zones,
            &subject_risk_overlays,
        );
        let base_local_policy_decision =
            apply_permission_mode_cap(policy_mode, local_policy_decision, access);
        let base_local_policy_decision =
            apply_policy_risk_overlay(policy_mode, base_local_policy_decision, operation, risk);
        let local_policy_decision = combine_modes(vec![
            base_local_policy_decision,
            external_directory_policy_decision,
        ]);
        let source_policy_decision = delegated_source_policy_decision;
        let mode = combine_modes(vec![
            local_policy_decision,
            network_policy_decision,
            source_policy_decision,
        ]);
        let confirmation = confirmation_for_risk(risk, &subject_zones).or_else(|| {
            (access == ToolAccess::Write && subject_zones.contains(&PathTrustZone::External))
                .then_some(PermissionConfirmation::TypePath)
        });
        let snapshot_required = matches!(risk, PermissionRisk::Destructive);
        Self {
            mode,
            access,
            network_effect,
            local_policy_decision,
            network_policy_decision,
            source_policy_decision,
            operation,
            risk,
            subjects,
            subject_zones,
            subject_risk_overlays,
            external_directory_required,
            confirmation,
            snapshot_required,
            command_permission_matches: Vec::new(),
            base_local_policy_decision,
            external_directory_policy_decision,
        }
    }

    pub(crate) fn recompute_mode(&mut self) {
        self.mode = combine_modes(vec![
            self.local_policy_decision,
            self.network_policy_decision,
            self.source_policy_decision,
        ]);
    }

    fn restrict_with(&mut self, constraint: Self) {
        debug_assert_eq!(self.access, constraint.access);
        debug_assert_eq!(self.network_effect, constraint.network_effect);
        debug_assert_eq!(self.operation, constraint.operation);
        debug_assert_eq!(self.subjects, constraint.subjects);
        debug_assert_eq!(self.subject_zones, constraint.subject_zones);

        self.base_local_policy_decision = combine_modes(vec![
            self.base_local_policy_decision,
            constraint.base_local_policy_decision,
        ]);
        self.external_directory_policy_decision = combine_modes(vec![
            self.external_directory_policy_decision,
            constraint.external_directory_policy_decision,
        ]);
        self.local_policy_decision = combine_modes(vec![
            self.base_local_policy_decision,
            self.external_directory_policy_decision,
        ]);
        self.network_policy_decision = combine_modes(vec![
            self.network_policy_decision,
            constraint.network_policy_decision,
        ]);
        self.source_policy_decision = combine_modes(vec![
            self.source_policy_decision,
            constraint.source_policy_decision,
        ]);
        self.external_directory_required |= constraint.external_directory_required;
        self.snapshot_required |= constraint.snapshot_required;
        if self.confirmation.is_none() {
            self.confirmation = constraint.confirmation;
        }
        self.command_permission_matches
            .extend(constraint.command_permission_matches);
        self.recompute_mode();
    }

    pub(crate) fn request_external_directory_interactive_approval(&mut self) {
        if !self.external_directory_required
            || self.external_directory_policy_decision != ApprovalMode::Deny
        {
            return;
        }
        self.external_directory_policy_decision = ApprovalMode::Ask;
        self.local_policy_decision = combine_modes(vec![
            self.base_local_policy_decision,
            self.external_directory_policy_decision,
        ]);
        self.recompute_mode();
    }
}

/// Policy evaluator that resolves allow/ask/deny for one tool call.
pub struct PermissionPolicy<'a> {
    config: &'a PermissionConfig,
    context: Option<&'a PermissionEvaluationContext>,
    command_patterns: Vec<CompiledCommandPermissionPattern<'a>>,
    rules: Vec<CompiledPermissionRule<'a>>,
    external_rules: Vec<CompiledExternalDirectoryRule<'a>>,
}

/// Permission evaluator for a primary run policy plus delegated narrowing constraints.
///
/// Each policy is evaluated independently for the concrete tool spec, operation, and subjects.
/// The resulting decisions are then combined monotonically, so a child role or profile cannot
/// relax a parent `Ask` or `Deny` decision.
pub struct PermissionPolicyChain<'a> {
    policies: Vec<PermissionPolicy<'a>>,
}

impl<'a> PermissionPolicyChain<'a> {
    /// Creates a decision-time policy chain from the primary config and runtime constraints.
    pub fn new_with_context(
        config: &'a PermissionConfig,
        context: &'a PermissionEvaluationContext,
    ) -> Self {
        let mut policies =
            Vec::with_capacity(context.delegated_policy_constraints.len().saturating_add(1));
        policies.push(PermissionPolicy::new_with_context(config, context));
        policies.extend(
            context
                .delegated_policy_constraints
                .iter()
                .map(|constraint| PermissionPolicy::new_with_context(constraint, context)),
        );
        Self { policies }
    }

    /// Resolves one tool call across every permission layer and returns the strictest decision.
    ///
    /// # Errors
    ///
    /// Returns an error when any configured subject glob is invalid.
    #[allow(clippy::too_many_arguments)]
    pub fn decide_with_operation_network_effect_and_default(
        &self,
        spec: &ToolSpec,
        tool_name: &str,
        access: ToolAccess,
        operation: ToolOperation,
        network_effect: Option<NetworkEffect>,
        subjects: Vec<ToolSubject>,
        tool_default_mode: Option<ApprovalMode>,
    ) -> Result<PermissionDecision> {
        let mut policies = self.policies.iter();
        let primary = policies
            .next()
            .expect("permission policy chain always contains the primary policy");
        let mut decision = primary.decide_with_operation_network_effect_and_default(
            spec,
            tool_name,
            access,
            operation,
            network_effect,
            subjects.clone(),
            tool_default_mode,
        )?;
        for policy in policies {
            let constraint = policy.decide_with_operation_network_effect_and_default(
                spec,
                tool_name,
                access,
                operation,
                network_effect,
                subjects.clone(),
                tool_default_mode,
            )?;
            decision.restrict_with(constraint);
        }
        Ok(decision)
    }
}

impl<'a> PermissionPolicy<'a> {
    /// Creates a policy evaluator from shared configuration.
    pub fn new(config: &'a PermissionConfig) -> Self {
        Self {
            config,
            context: None,
            command_patterns: compile_command_permission_patterns(&config.commands),
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
            command_patterns: compile_command_permission_patterns(&config.commands),
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
        self.decide_with_operation_network_effect_and_default(
            spec,
            tool_name,
            access,
            infer_tool_operation(tool_name, access),
            spec.network_effect,
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
        spec: &ToolSpec,
        tool_name: &str,
        access: ToolAccess,
        operation: ToolOperation,
        subjects: Vec<ToolSubject>,
        tool_default_mode: Option<ApprovalMode>,
    ) -> Result<PermissionDecision> {
        self.decide_with_operation_network_effect_and_default(
            spec,
            tool_name,
            access,
            operation,
            spec.network_effect,
            subjects,
            tool_default_mode,
        )
    }

    /// Resolves one tool call across independent local, network, and source policy facets.
    ///
    /// # Errors
    ///
    /// Returns an error when one configured subject glob is invalid.
    pub fn decide_with_operation_network_effect_and_default(
        &self,
        _spec: &ToolSpec,
        tool_name: &str,
        access: ToolAccess,
        operation: ToolOperation,
        network_effect: Option<NetworkEffect>,
        subjects: Vec<ToolSubject>,
        tool_default_mode: Option<ApprovalMode>,
    ) -> Result<PermissionDecision> {
        let subject_analyses = self.classify_subject_trust_analyses(&subjects);
        let subject_zones = subject_analyses
            .iter()
            .map(|analysis| analysis.zone)
            .collect::<Vec<_>>();
        let subject_risk_overlays = collect_path_risk_overlays(&subject_analyses);
        let external_directory_required = subjects
            .iter()
            .any(subject_requires_external_directory_gate)
            && !self.config.external_directory.enabled;
        let external_directory_policy_decision = {
            let external_modes = subjects
                .iter()
                .filter(|subject| subject_requires_external_directory_gate(subject))
                .map(|subject| self.decide_external_subject(subject))
                .collect::<Result<Vec<_>>>()?;
            if external_modes.is_empty() {
                ApprovalMode::Allow
            } else {
                combine_modes(external_modes)
            }
        };
        let command_decision = self.decide_command_permissions(tool_name, &subjects);
        let command_mode = command_decision.mode;
        let subject_modes = if subjects.is_empty() {
            vec![self.decide_one_subject(tool_name, access, operation, command_mode, None, None)?]
        } else {
            subjects
                .iter()
                .zip(subject_zones.iter().copied())
                .map(|(subject, zone)| {
                    self.decide_one_subject(
                        tool_name,
                        access,
                        operation,
                        command_mode,
                        Some(subject),
                        Some(zone),
                    )
                })
                .collect::<Result<Vec<_>>>()?
        };

        let mut local_policy_decision = combine_modes(subject_modes);
        if let Some(cap_mode) = self
            .context
            .and_then(|context| context.effective_policy_cap.as_ref())
            .map(|cap| cap.mode)
        {
            local_policy_decision = combine_modes(vec![local_policy_decision, cap_mode]);
        }

        let delegated_source_policy_decision = {
            let source_modes = if subjects.is_empty() {
                vec![
                    if self.explicit_tool_policy_mode(tool_name, None)?.is_some() {
                        ApprovalMode::Allow
                    } else {
                        tool_default_mode.unwrap_or(ApprovalMode::Allow)
                    },
                ]
            } else {
                subjects
                    .iter()
                    .map(|subject| {
                        self.explicit_tool_policy_mode(tool_name, Some(subject))
                            .map(|mode| {
                                if mode.is_some() {
                                    ApprovalMode::Allow
                                } else {
                                    tool_default_mode.unwrap_or(ApprovalMode::Allow)
                                }
                            })
                    })
                    .collect::<Result<Vec<_>>>()?
            };
            combine_modes(source_modes)
        };
        let network_policy_decision = evaluate_network_policy(
            self.config.mode,
            network_effect,
            self.context
                .map_or(NetworkPolicy::Allow, |context| context.network_policy),
        );

        let mut decision = PermissionDecision::new_with_policy_facets_operation_zones_and_overlays(
            self.config.mode,
            local_policy_decision,
            network_policy_decision,
            delegated_source_policy_decision,
            external_directory_policy_decision,
            network_effect,
            operation,
            access,
            subjects,
            subject_zones,
            subject_risk_overlays,
            external_directory_required,
        );
        decision.command_permission_matches = command_decision.matches;
        Ok(decision)
    }

    fn classify_subject_trust_analyses(&self, subjects: &[ToolSubject]) -> Vec<PathTrustAnalysis> {
        subjects
            .iter()
            .map(|subject| {
                self.context.map_or_else(
                    || classify_path_trust_analysis(subject),
                    |context| classify_path_trust_analysis_with_context(subject, context),
                )
            })
            .collect()
    }

    fn decide_one_subject(
        &self,
        tool_name: &str,
        access: ToolAccess,
        operation: ToolOperation,
        command_mode: Option<ApprovalMode>,
        subject: Option<&ToolSubject>,
        zone: Option<PathTrustZone>,
    ) -> Result<ApprovalMode> {
        let mut mode = self.config.mode.baseline_for(access, operation, zone);

        let tool_mode = self.config.tools.get(tool_name).copied();
        if let Some(tool_mode) = tool_mode {
            mode = tool_mode;
        }

        let explicit_tool_policy_mode = self.explicit_tool_policy_mode(tool_name, subject)?;
        let tool_policy_mode = explicit_tool_policy_mode.unwrap_or(mode);
        let tool_policy_mode = match (command_mode, explicit_tool_policy_mode) {
            (Some(command_mode), Some(explicit_mode)) => {
                combine_modes(vec![explicit_mode, command_mode])
            }
            (Some(command_mode), None) => command_mode,
            (None, _) => tool_policy_mode,
        };

        Ok(tool_policy_mode)
    }

    fn explicit_tool_policy_mode(
        &self,
        tool_name: &str,
        subject: Option<&ToolSubject>,
    ) -> Result<Option<ApprovalMode>> {
        let matching_rule_modes = self
            .rules
            .iter()
            .filter_map(|compiled| match compiled.matches(tool_name, subject) {
                Ok(true) => Some(Ok(compiled.rule.mode)),
                Ok(false) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(matching_rule_modes
            .last()
            .copied()
            .or_else(|| self.config.tools.get(tool_name).copied()))
    }

    fn decide_command_permissions(
        &self,
        tool_name: &str,
        subjects: &[ToolSubject],
    ) -> CommandPermissionDecision {
        if !command_permission_tool_name_supported(tool_name) {
            return CommandPermissionDecision::default();
        }
        let matches = subjects
            .iter()
            .filter(|subject| subject.kind == ToolSubjectKind::Command)
            .flat_map(|subject| self.match_command_subject(subject))
            .collect::<Vec<_>>();
        let mode = if matches.is_empty() {
            None
        } else {
            Some(combine_modes(
                matches.iter().map(|item| item.group.action()).collect(),
            ))
        };
        CommandPermissionDecision { mode, matches }
    }

    fn match_command_subject(&self, subject: &ToolSubject) -> Vec<CommandPermissionMatch> {
        let original = normalize_command_pattern_subject(&subject.original);
        let normalized = normalize_command_pattern_subject(&subject.normalized);
        self.command_patterns
            .iter()
            .filter_map(|compiled| {
                let command = if compiled.matches(&original) {
                    Some(original.as_str())
                } else if normalized != original && compiled.matches(&normalized) {
                    Some(normalized.as_str())
                } else {
                    None
                }?;
                Some(CommandPermissionMatch {
                    group: compiled.group,
                    pattern: compiled.pattern.to_owned(),
                    command: command.to_owned(),
                })
            })
            .collect()
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

        Ok(matching_rule_modes
            .last()
            .copied()
            .unwrap_or(config.default_mode))
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
        "spawn_agent" | "spawn_agents" | "request_task_discovery" => ToolOperation::SpawnAgent,
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
        },
    }
}

/// Classifies a path subject into a trust zone using conservative built-in defaults.
pub fn classify_path_trust_zone(subject: &ToolSubject) -> PathTrustZone {
    classify_path_trust_analysis(subject).zone
}

/// Classifies a path subject into a trust zone and independent risk overlays.
pub fn classify_path_trust_analysis(subject: &ToolSubject) -> PathTrustAnalysis {
    if subject.kind != ToolSubjectKind::Path {
        return PathTrustAnalysis {
            zone: PathTrustZone::Unknown,
            overlays: Vec::new(),
        };
    }
    if subject.scope == ToolSubjectScope::External {
        return PathTrustAnalysis {
            zone: PathTrustZone::External,
            overlays: path_risk_overlays(subject),
        };
    }

    let normalized = subject.normalized.trim_start_matches("./");
    if normalized.trim().is_empty() {
        return PathTrustAnalysis {
            zone: PathTrustZone::Unknown,
            overlays: Vec::new(),
        };
    }
    let overlays = path_risk_overlays(subject);
    if path_is_under(normalized, ".git") {
        return PathTrustAnalysis {
            zone: PathTrustZone::WorkspaceGitMetadata,
            overlays,
        };
    }
    if normalized == ".sigil" {
        return PathTrustAnalysis {
            zone: PathTrustZone::WorkspaceRuntimeState,
            overlays,
        };
    }
    if path_is_under_any(normalized, WORKSPACE_RUNTIME_STATE_PATHS) {
        return PathTrustAnalysis {
            zone: PathTrustZone::WorkspaceRuntimeState,
            overlays,
        };
    }
    if path_is_under_any(normalized, WORKSPACE_PROJECT_ASSET_PATHS) {
        return PathTrustAnalysis {
            zone: PathTrustZone::WorkspaceProjectAsset,
            overlays,
        };
    }
    if path_is_under_any(normalized, WORKSPACE_DOC_PATHS) {
        return PathTrustAnalysis {
            zone: PathTrustZone::WorkspaceDocs,
            overlays,
        };
    }
    if workspace_config_secret_path(normalized) {
        return PathTrustAnalysis {
            zone: PathTrustZone::WorkspaceConfigSecret,
            overlays,
        };
    }
    PathTrustAnalysis {
        zone: PathTrustZone::WorkspaceSource,
        overlays,
    }
}

/// Classifies a path subject into a trust zone using the active runtime path context.
pub fn classify_path_trust_zone_with_context(
    subject: &ToolSubject,
    context: &PermissionEvaluationContext,
) -> PathTrustZone {
    classify_path_trust_analysis_with_context(subject, context).zone
}

/// Classifies a path subject using the active runtime path context and risk overlays.
pub fn classify_path_trust_analysis_with_context(
    subject: &ToolSubject,
    context: &PermissionEvaluationContext,
) -> PathTrustAnalysis {
    if subject.kind != ToolSubjectKind::Path {
        return PathTrustAnalysis {
            zone: PathTrustZone::Unknown,
            overlays: Vec::new(),
        };
    }
    if subject.scope == ToolSubjectScope::External {
        return PathTrustAnalysis {
            zone: PathTrustZone::External,
            overlays: path_risk_overlays(subject),
        };
    }

    let Some(subject_path) = subject_path_for_context(subject, context) else {
        return classify_path_trust_analysis(subject);
    };
    let overlays = path_risk_overlays(subject);
    if path_starts_with_any_context_root(&subject_path, &context.runtime_state_roots, context) {
        return PathTrustAnalysis {
            zone: PathTrustZone::WorkspaceRuntimeState,
            overlays,
        };
    }
    if path_starts_with_any_context_root(&subject_path, &context.project_asset_roots, context) {
        return PathTrustAnalysis {
            zone: PathTrustZone::WorkspaceProjectAsset,
            overlays,
        };
    }
    if path_starts_with_any_context_root(&subject_path, &context.user_state_roots, context) {
        return PathTrustAnalysis {
            zone: PathTrustZone::UserState,
            overlays,
        };
    }
    if path_starts_with_any_context_root(&subject_path, &context.user_cache_roots, context) {
        return PathTrustAnalysis {
            zone: PathTrustZone::UserCache,
            overlays,
        };
    }

    let workspace_root = normalize_policy_path(&context.workspace_root);
    if !workspace_root.as_os_str().is_empty() && !subject_path.starts_with(&workspace_root) {
        return PathTrustAnalysis {
            zone: PathTrustZone::External,
            overlays,
        };
    }

    let built_in_subject = subject_path
        .strip_prefix(&workspace_root)
        .ok()
        .map(|relative| {
            let mut normalized = relative.to_string_lossy().replace('\\', "/");
            if normalized.is_empty() {
                normalized = ".".to_owned();
            }
            ToolSubject::path_with_scope(
                subject.original.clone(),
                normalized,
                subject.canonical_path.clone(),
                ToolSubjectScope::Workspace,
            )
        });
    let built_in = built_in_subject.as_ref().map_or_else(
        || classify_path_trust_analysis(subject),
        classify_path_trust_analysis,
    );
    PathTrustAnalysis {
        zone: built_in.zone,
        overlays,
    }
}

fn subject_requires_external_directory_gate(subject: &ToolSubject) -> bool {
    subject.kind == ToolSubjectKind::Path && subject.scope == ToolSubjectScope::External
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
    overlays: &[PathRiskOverlay],
) -> PermissionRisk {
    derive_permission_risk_with_network_effect(access, None, operation, zones, overlays)
}

/// Derives permission risk while preserving independent network mutation uncertainty.
pub fn derive_permission_risk_with_network_effect(
    access: ToolAccess,
    network_effect: Option<NetworkEffect>,
    operation: ToolOperation,
    zones: &[PathTrustZone],
    overlays: &[PathRiskOverlay],
) -> PermissionRisk {
    if (zones.iter().any(|zone| {
        matches!(
            zone,
            PathTrustZone::WorkspaceGitMetadata
                | PathTrustZone::WorkspaceRuntimeState
                | PathTrustZone::WorkspaceConfigSecret
                | PathTrustZone::UserState
                | PathTrustZone::UserCache
        )
    }) || overlays.contains(&PathRiskOverlay::SensitiveName))
        && access != ToolAccess::Read
    {
        return PermissionRisk::Protected;
    }

    if matches!(
        operation,
        ToolOperation::DeleteFile
            | ToolOperation::DeleteDirectory
            | ToolOperation::RecursiveDelete
            | ToolOperation::ApplyChangeSet
            | ToolOperation::ExecuteMutatingCommand
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

    if operation == ToolOperation::ExecuteWorkspaceCheckCommand {
        return PermissionRisk::Medium;
    }

    let local_risk = match access {
        ToolAccess::Read => PermissionRisk::Low,
        ToolAccess::Write => PermissionRisk::Medium,
        ToolAccess::Execute => PermissionRisk::High,
    };
    if network_effect.is_some() {
        local_risk.max(PermissionRisk::High)
    } else {
        local_risk
    }
}

/// Applies safety overlays that no allow source can bypass.
pub fn apply_risk_overlay(mode: ApprovalMode, risk: PermissionRisk) -> ApprovalMode {
    apply_operation_risk_overlay(mode, ToolOperation::Read, risk)
}

/// Returns true when an interactive approval can safely be widened to a session-local grant.
pub fn tool_approval_session_grant_available(decision: &PermissionDecision) -> bool {
    tool_approval_session_grant_shape_for_facets(
        decision.access,
        decision.network_effect,
        decision.operation,
        decision.risk,
        &decision.subjects,
        &decision.subject_zones,
        decision.confirmation.as_ref(),
        decision.snapshot_required,
        decision.local_policy_decision,
        decision.network_policy_decision,
        decision.source_policy_decision,
    )
    .is_some()
}

/// Returns true when an approval can safely become a bounded session-local grant.
#[allow(clippy::too_many_arguments)]
pub fn tool_approval_session_grant_available_for_facets(
    access: ToolAccess,
    network_effect: Option<NetworkEffect>,
    operation: ToolOperation,
    risk: PermissionRisk,
    subjects: &[ToolSubject],
    zones: &[PathTrustZone],
    confirmation: Option<&PermissionConfirmation>,
    snapshot_required: bool,
    local_policy_decision: ApprovalMode,
    network_policy_decision: ApprovalMode,
    source_policy_decision: ApprovalMode,
) -> bool {
    tool_approval_session_grant_shape_for_facets(
        access,
        network_effect,
        operation,
        risk,
        subjects,
        zones,
        confirmation,
        snapshot_required,
        local_policy_decision,
        network_policy_decision,
        source_policy_decision,
    )
    .is_some()
}

pub(crate) fn tool_approval_session_grant_shape(
    decision: &PermissionDecision,
) -> Option<ToolApprovalSessionGrantShape> {
    tool_approval_session_grant_shape_for_facets(
        decision.access,
        decision.network_effect,
        decision.operation,
        decision.risk,
        &decision.subjects,
        &decision.subject_zones,
        decision.confirmation.as_ref(),
        decision.snapshot_required,
        decision.local_policy_decision,
        decision.network_policy_decision,
        decision.source_policy_decision,
    )
}

#[allow(clippy::too_many_arguments)]
fn tool_approval_session_grant_shape_for_facets(
    access: ToolAccess,
    network_effect: Option<NetworkEffect>,
    operation: ToolOperation,
    risk: PermissionRisk,
    subjects: &[ToolSubject],
    zones: &[PathTrustZone],
    confirmation: Option<&PermissionConfirmation>,
    snapshot_required: bool,
    local_policy_decision: ApprovalMode,
    network_policy_decision: ApprovalMode,
    source_policy_decision: ApprovalMode,
) -> Option<ToolApprovalSessionGrantShape> {
    if source_policy_decision != ApprovalMode::Allow
        || matches!(local_policy_decision, ApprovalMode::Deny)
        || matches!(network_policy_decision, ApprovalMode::Deny)
    {
        return None;
    }

    let mut facets = Vec::new();
    if local_policy_decision == ApprovalMode::Ask {
        facets.push(ToolApprovalSessionGrantFacet::Local);
    }
    if network_policy_decision == ApprovalMode::Ask {
        facets.push(ToolApprovalSessionGrantFacet::Network);
    }
    if facets.is_empty() {
        return None;
    }

    let network_read_scope = network_read_session_grant_scope(
        access,
        network_effect,
        operation,
        risk,
        subjects,
        confirmation,
        snapshot_required,
    );
    if network_policy_decision == ApprovalMode::Ask {
        let scope = network_read_scope?;
        return Some(ToolApprovalSessionGrantShape { facets, scope });
    }
    if let Some(scope) = network_read_scope {
        return Some(ToolApprovalSessionGrantShape { facets, scope });
    }

    let network_risk_is_consistent = network_effect.is_none() || risk >= PermissionRisk::High;
    (network_policy_decision == ApprovalMode::Allow
        && network_risk_is_consistent
        && tool_approval_session_grant_available_for_parts(
            access,
            operation,
            risk,
            subjects,
            zones,
            confirmation,
            snapshot_required,
        ))
    .then_some(ToolApprovalSessionGrantShape {
        facets,
        scope: ToolApprovalSessionGrantScope::ExactSubjects,
    })
}

fn network_read_session_grant_scope(
    access: ToolAccess,
    network_effect: Option<NetworkEffect>,
    operation: ToolOperation,
    risk: PermissionRisk,
    subjects: &[ToolSubject],
    confirmation: Option<&PermissionConfirmation>,
    snapshot_required: bool,
) -> Option<ToolApprovalSessionGrantScope> {
    if access != ToolAccess::Read
        || network_effect != Some(NetworkEffect::Read)
        || operation != ToolOperation::NetworkRequest
        || risk != PermissionRisk::High
        || confirmation.is_some()
        || snapshot_required
        || subjects.is_empty()
    {
        return None;
    }
    if subjects.iter().all(|subject| {
        subject.kind == ToolSubjectKind::NetworkEndpoint && !subject.normalized.trim().is_empty()
    }) {
        return Some(ToolApprovalSessionGrantScope::NetworkReadTool);
    }
    subjects
        .iter()
        .all(stable_network_read_subject)
        .then_some(ToolApprovalSessionGrantScope::ExactSubjects)
}

fn stable_network_read_subject(subject: &ToolSubject) -> bool {
    match subject.kind {
        ToolSubjectKind::McpTool => !subject.normalized.trim().is_empty(),
        ToolSubjectKind::McpTrustClass => !subject.original.trim().is_empty(),
        _ => false,
    }
}

pub fn evaluate_network_policy(
    permission_mode: PermissionMode,
    effect: Option<NetworkEffect>,
    policy: NetworkPolicy,
) -> ApprovalMode {
    match effect {
        None => ApprovalMode::Allow,
        Some(NetworkEffect::Read) => policy.approval_mode(),
        Some(NetworkEffect::Mutate | NetworkEffect::Unknown)
            if permission_mode == PermissionMode::ReadOnly =>
        {
            ApprovalMode::Deny
        }
        Some(NetworkEffect::Mutate | NetworkEffect::Unknown) => policy.approval_mode(),
    }
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
            | ToolOperation::ExecuteWorkspaceCheckCommand
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
            (matches!(
                operation,
                ToolOperation::ExecuteReadOnlyCommand | ToolOperation::ExecuteWorkspaceCheckCommand
            ) || exact_command_grant_available)
                && !subject.normalized.trim().is_empty()
        }
        ToolSubjectKind::McpTrustClass => !subject.original.trim().is_empty(),
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

fn apply_policy_risk_overlay(
    policy_mode: PermissionMode,
    mode: ApprovalMode,
    operation: ToolOperation,
    risk: PermissionRisk,
) -> ApprovalMode {
    if policy_mode == PermissionMode::DangerFullAccess {
        return mode;
    }
    apply_operation_risk_overlay(mode, operation, risk)
}

fn apply_permission_mode_cap(
    policy_mode: PermissionMode,
    mode: ApprovalMode,
    access: ToolAccess,
) -> ApprovalMode {
    match policy_mode {
        PermissionMode::ReadOnly if access != ToolAccess::Read => ApprovalMode::Deny,
        PermissionMode::ReadOnly
        | PermissionMode::Manual
        | PermissionMode::AutoEdit
        | PermissionMode::DangerFullAccess => mode,
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

fn collect_path_risk_overlays(analyses: &[PathTrustAnalysis]) -> Vec<PathRiskOverlay> {
    analyses
        .iter()
        .flat_map(|analysis| analysis.overlays.iter().copied())
        .fold(Vec::new(), |mut overlays, overlay| {
            if !overlays.contains(&overlay) {
                overlays.push(overlay);
            }
            overlays
        })
}

fn path_risk_overlays(subject: &ToolSubject) -> Vec<PathRiskOverlay> {
    if subject.kind != ToolSubjectKind::Path {
        return Vec::new();
    }
    let normalized = subject.normalized.trim_start_matches("./");
    workspace_config_secret_path(normalized)
        .then_some(PathRiskOverlay::SensitiveName)
        .into_iter()
        .collect()
}

fn workspace_config_secret_path(path: &str) -> bool {
    if path == "sigil.toml" {
        return true;
    }
    Path::new(path).components().any(|component| {
        let Component::Normal(part) = component else {
            return false;
        };
        let name = part.to_string_lossy().to_ascii_lowercase();
        name == ".env"
            || name.starts_with(".env.")
            || name == "credentials"
            || name.starts_with("credentials.")
            || name == "secret"
            || name.starts_with("secret.")
            || name == "secrets"
            || name.starts_with("secrets.")
    })
}

#[derive(Debug, Clone, Default)]
struct CommandPermissionDecision {
    mode: Option<ApprovalMode>,
    matches: Vec<CommandPermissionMatch>,
}

struct CompiledCommandPermissionPattern<'a> {
    pattern: &'a str,
    group: CommandPermissionGroup,
}

impl<'a> CompiledCommandPermissionPattern<'a> {
    fn new(pattern: &'a str, group: CommandPermissionGroup) -> Self {
        Self { pattern, group }
    }

    fn matches(&self, command: &str) -> bool {
        command_pattern_matches(self.pattern, command)
    }
}

fn compile_command_permission_patterns(
    config: &CommandPermissionConfig,
) -> Vec<CompiledCommandPermissionPattern<'_>> {
    config
        .deny
        .iter()
        .map(|pattern| CompiledCommandPermissionPattern::new(pattern, CommandPermissionGroup::Deny))
        .chain(config.ask.iter().map(|pattern| {
            CompiledCommandPermissionPattern::new(pattern, CommandPermissionGroup::Ask)
        }))
        .chain(config.allow.iter().map(|pattern| {
            CompiledCommandPermissionPattern::new(pattern, CommandPermissionGroup::Allow)
        }))
        .collect()
}

fn command_permission_tool_name_supported(tool_name: &str) -> bool {
    matches!(tool_name, "bash" | "terminal_start" | "terminal_input")
}

fn normalize_command_permission_patterns(patterns: Vec<String>) -> Vec<String> {
    patterns
        .into_iter()
        .map(|pattern| normalize_command_pattern_subject(&pattern))
        .collect()
}

fn normalize_command_pattern_subject(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn validate_command_permission_config(
    config: &CommandPermissionConfig,
) -> std::result::Result<(), String> {
    let mut seen = BTreeMap::<String, CommandPermissionGroup>::new();
    for (group, patterns) in [
        (CommandPermissionGroup::Allow, &config.allow),
        (CommandPermissionGroup::Ask, &config.ask),
        (CommandPermissionGroup::Deny, &config.deny),
    ] {
        for pattern in patterns {
            if pattern.is_empty() {
                return Err(format!(
                    "permission.commands.{} contains an empty pattern",
                    group.as_str()
                ));
            }
            if pattern.contains('\n') || pattern.contains('\r') {
                return Err(format!(
                    "permission.commands.{} pattern {pattern:?} must be one line",
                    group.as_str()
                ));
            }
            if let Some(previous) = seen.insert(pattern.clone(), group)
                && previous != group
            {
                return Err(format!(
                    "permission.commands pattern {pattern:?} appears in both {} and {}",
                    previous.as_str(),
                    group.as_str()
                ));
            }
        }
    }
    Ok(())
}

fn command_pattern_matches(pattern: &str, command: &str) -> bool {
    let pattern_chars = pattern.chars().collect::<Vec<_>>();
    let command_chars = command.chars().collect::<Vec<_>>();
    let mut pattern_index = 0usize;
    let mut command_index = 0usize;
    let mut star_pattern_index: Option<usize> = None;
    let mut star_command_index = 0usize;

    while command_index < command_chars.len() {
        if pattern_index < pattern_chars.len()
            && (pattern_chars[pattern_index] == '?'
                || pattern_chars[pattern_index] == command_chars[command_index])
        {
            pattern_index += 1;
            command_index += 1;
        } else if pattern_index < pattern_chars.len() && pattern_chars[pattern_index] == '*' {
            star_pattern_index = Some(pattern_index);
            pattern_index += 1;
            star_command_index = command_index;
        } else if let Some(star_index) = star_pattern_index {
            pattern_index = star_index + 1;
            star_command_index += 1;
            command_index = star_command_index;
        } else {
            return false;
        }
    }

    while pattern_index < pattern_chars.len() && pattern_chars[pattern_index] == '*' {
        pattern_index += 1;
    }

    pattern_index == pattern_chars.len()
}

struct CompiledPermissionRule<'a> {
    rule: &'a PermissionRule,
    tool_matcher: CompiledMatcher,
    subject_matcher: CompiledMatcher,
}

impl<'a> CompiledPermissionRule<'a> {
    fn new(rule: &'a PermissionRule) -> Self {
        let tool_matcher = match rule.tool_name.as_deref() {
            Some(tool_name) => compile_permission_tool_glob(tool_name),
            None => CompiledMatcher::Any,
        };
        let subject_matcher = match rule.subject_glob.as_deref() {
            Some(subject_glob) => compile_permission_glob(subject_glob),
            None => CompiledMatcher::Any,
        };
        Self {
            rule,
            tool_matcher,
            subject_matcher,
        }
    }

    fn matches(&self, tool_name: &str, subject: Option<&ToolSubject>) -> Result<bool> {
        if !self.tool_matcher.is_match(tool_name)? {
            return Ok(false);
        }
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

fn compile_permission_tool_glob(tool_glob: &str) -> CompiledMatcher {
    Glob::new(tool_glob).map_or_else(
        |error| {
            CompiledMatcher::Invalid(format!("invalid permission tool glob {tool_glob}: {error}"))
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
        .or_else(|| {
            if cfg!(windows) {
                std::env::var_os("USERPROFILE")
            } else {
                None
            }
        })
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
#[path = "tests/network_permission_tests.rs"]
mod network_tests;
#[cfg(test)]
#[path = "tests/permission_tests.rs"]
mod tests;
