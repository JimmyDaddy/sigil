use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
    sync::OnceLock,
};

use anyhow::{Result, anyhow};
use globset::{Glob, GlobMatcher};
use serde::{Deserialize, Serialize, de};

use crate::tool::{
    NetworkEffect, ToolAccess, ToolCategory, ToolSpec, ToolSubject, ToolSubjectKind,
    ToolSubjectScope,
};
use crate::{NetworkContainment, ToolAnalysisStatus, ToolPermissionEffect, ToolPermissionPlanV2};

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
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolApprovalSessionGrantScope {
    #[default]
    ExactSubjects,
    /// Same tool and read-only network operation; destination controls still run per call.
    NetworkReadTool,
    /// Unknown-family shell commands sharing the derived first-two-token prefix; the prefix is
    /// the whitespace-normalized `program subcommand` pair and matches on token boundaries.
    CommandFamily { prefix: String },
}

/// Stable, provider-neutral reason why a bounded session-local grant is unavailable.
///
/// The code is intentionally closed and contains no provider payload or unbounded diagnostic
/// detail. Product surfaces map it to localized copy instead of deriving policy semantics from
/// the tool call.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolApprovalSessionGrantUnavailableReasonCode {
    AnalysisIncomplete,
    SemanticScopeUnavailable,
    NonGrantableEffect,
    ContainmentBindingUnavailable,
    PolicyDecisionNotGrantable,
    NoReusableApprovalFacet,
    NetworkScopeNotGrantable,
    ConfirmationRequired,
    SnapshotRequired,
    SubjectScopeUnavailable,
    RiskNotGrantable,
    ExternalMutation,
    OperationNotGrantable,
}

/// Typed reason carried by the shared approval contract when session authority cannot be granted.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolApprovalSessionGrantUnavailableReason {
    pub code: ToolApprovalSessionGrantUnavailableReasonCode,
}

/// Invariant-preserving result of the shared session-grant availability calculation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub struct ToolApprovalSessionGrantAvailability {
    available: bool,
    unavailable_reason: Option<ToolApprovalSessionGrantUnavailableReason>,
}

impl ToolApprovalSessionGrantAvailability {
    const fn available() -> Self {
        Self {
            available: true,
            unavailable_reason: None,
        }
    }

    const fn unavailable(code: ToolApprovalSessionGrantUnavailableReasonCode) -> Self {
        Self {
            available: false,
            unavailable_reason: Some(ToolApprovalSessionGrantUnavailableReason { code }),
        }
    }

    pub const fn is_available(self) -> bool {
        self.available
    }

    pub const fn unavailable_reason(self) -> Option<ToolApprovalSessionGrantUnavailableReason> {
        self.unavailable_reason
    }
}

impl ToolApprovalSessionGrantScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ExactSubjects => "exact_subjects",
            Self::NetworkReadTool => "network_read_tool",
            Self::CommandFamily { .. } => "command_family",
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
    ResizeTerminalTask,
    CancelTerminalTask,
    NetworkRequest,
    SpawnAgent,
    MessageAgent,
    CloseAgent,
    LoadSkill,
    RememberMemory,
    ForgetMemory,
    InvokePlugin,
}

impl ToolOperation {
    /// Returns the stable label used by permission plans and diagnostics.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Search => "search",
            Self::CreateFile => "create_file",
            Self::EditFile => "edit_file",
            Self::OverwriteFile => "overwrite_file",
            Self::DeleteFile => "delete_file",
            Self::RenamePath => "rename_path",
            Self::CreateDirectory => "create_directory",
            Self::DeleteDirectory => "delete_directory",
            Self::RecursiveDelete => "recursive_delete",
            Self::ApplyChangeSet => "apply_change_set",
            Self::ExecuteReadOnlyCommand => "execute_read_only_command",
            Self::ExecuteWorkspaceCheckCommand => "execute_workspace_check_command",
            Self::ExecuteMutatingCommand => "execute_mutating_command",
            Self::ExecuteUnknownCommand => "execute_unknown_command",
            Self::ExecuteDestructiveCommand => "execute_destructive_command",
            Self::SendTerminalInput => "send_terminal_input",
            Self::ResizeTerminalTask => "resize_terminal_task",
            Self::CancelTerminalTask => "cancel_terminal_task",
            Self::NetworkRequest => "network_request",
            Self::SpawnAgent => "spawn_agent",
            Self::MessageAgent => "message_agent",
            Self::CloseAgent => "close_agent",
            Self::LoadSkill => "load_skill",
            Self::RememberMemory => "remember_memory",
            Self::ForgetMemory => "forget_memory",
            Self::InvokePlugin => "invoke_plugin",
        }
    }
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
    RuntimeScratch,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HardSafetyPathTarget {
    FilesystemRoot,
    UserHome,
    SystemRoot,
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

/// Policy source that contributed one explainable V2 decision reason.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecisionSource {
    HardSafety,
    ManagedRule,
    DelegatedRule,
    UserRule,
    SessionGrant,
    SandboxSubstitution,
    PermissionModeDefault,
    ToolDefault,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PermissionDecisionReason {
    pub source: PermissionDecisionSource,
    pub code: String,
    pub detail: String,
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
    pub reasons: Vec<PermissionDecisionReason>,
    base_local_policy_decision: ApprovalMode,
    external_directory_policy_decision: ApprovalMode,
    danger_full_access: bool,
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
        let risk = derive_permission_risk_with_subject_access(
            access,
            network_effect,
            operation,
            &subject_zones,
            &subject_risk_overlays,
            &subjects,
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
        let mut reasons = vec![PermissionDecisionReason {
            source: PermissionDecisionSource::PermissionModeDefault,
            code: "permission_mode_default".to_owned(),
            detail: format!("{} produced {}", policy_mode.as_str(), mode.as_str()),
        }];
        if risk == PermissionRisk::Protected {
            reasons.push(PermissionDecisionReason {
                source: PermissionDecisionSource::HardSafety,
                code: "protected_target".to_owned(),
                detail: "protected targets cannot be authorized by ordinary permission modes"
                    .to_owned(),
            });
        }
        let mut decision = Self {
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
            reasons,
            base_local_policy_decision,
            external_directory_policy_decision,
            danger_full_access: policy_mode == PermissionMode::DangerFullAccess,
        };
        decision.enforce_critical_path_hard_safety();
        decision.suppress_danger_full_access_asks();
        decision
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
        self.reasons.extend(constraint.reasons);
        self.recompute_mode();
    }

    fn suppress_danger_full_access_asks(&mut self) {
        if !self.danger_full_access {
            return;
        }
        let mut suppressed = false;
        for decision in [
            &mut self.base_local_policy_decision,
            &mut self.external_directory_policy_decision,
            &mut self.network_policy_decision,
            &mut self.source_policy_decision,
        ] {
            if *decision == ApprovalMode::Ask {
                *decision = ApprovalMode::Allow;
                suppressed = true;
            }
        }
        self.local_policy_decision = combine_modes(vec![
            self.base_local_policy_decision,
            self.external_directory_policy_decision,
        ]);
        self.recompute_mode();
        if suppressed {
            self.reasons.push(PermissionDecisionReason {
                source: PermissionDecisionSource::PermissionModeDefault,
                code: "danger_full_access_ask_suppressed".to_owned(),
                detail: "danger-full-access converted interactive ask facets to allow; deny facets and hard safety remain authoritative".to_owned(),
            });
        }
    }

    pub(crate) fn danger_full_access_network_authorized(&self) -> bool {
        self.danger_full_access
            && self.network_effect.is_some()
            && self.network_policy_decision == ApprovalMode::Allow
    }

    fn restrict_for_incomplete_analysis(
        &mut self,
        policy_mode: PermissionMode,
        analysis: &ToolAnalysisStatus,
    ) {
        if analysis.is_complete() {
            return;
        }
        if policy_mode == PermissionMode::DangerFullAccess {
            self.reasons.push(PermissionDecisionReason {
                source: PermissionDecisionSource::PermissionModeDefault,
                code: "danger_full_access_incomplete_analysis_allowed".to_owned(),
                detail: "danger-full-access keeps incomplete local analysis non-interactive; independent hard safety and policy facets still apply".to_owned(),
            });
            return;
        }
        self.base_local_policy_decision =
            combine_modes(vec![self.base_local_policy_decision, ApprovalMode::Ask]);
        self.local_policy_decision = combine_modes(vec![
            self.base_local_policy_decision,
            self.external_directory_policy_decision,
        ]);
        self.reasons.push(PermissionDecisionReason {
            source: PermissionDecisionSource::HardSafety,
            code: "deterministic_analysis_incomplete".to_owned(),
            detail: "incomplete shell analysis requires an exact interactive decision".to_owned(),
        });
        self.recompute_mode();
    }

    /// Applies the monotone safety floor derived from the plan's declared effects.
    ///
    /// `access` and `operation` are useful routing labels, but they are not authoritative safety
    /// facts. A producer that under-declares either label must therefore not be able to lower the
    /// risk or approval action implied by effects such as deletion, dynamic execution, remote
    /// mutation, or credential access.
    fn restrict_for_permission_effects(
        &mut self,
        policy_mode: PermissionMode,
        effects: &BTreeSet<ToolPermissionEffect>,
    ) {
        let floor = permission_effect_policy_floor(effects, self.access, self.operation);
        self.risk = self.risk.max(floor.risk);
        if policy_mode != PermissionMode::DangerFullAccess
            || floor.local_policy_decision == ApprovalMode::Deny
        {
            self.base_local_policy_decision = combine_modes(vec![
                self.base_local_policy_decision,
                floor.local_policy_decision,
            ]);
        }
        self.local_policy_decision = combine_modes(vec![
            self.base_local_policy_decision,
            self.external_directory_policy_decision,
        ]);
        self.snapshot_required |= floor.snapshot_required;
        if self.risk == PermissionRisk::Protected {
            self.base_local_policy_decision = ApprovalMode::Deny;
            self.local_policy_decision = ApprovalMode::Deny;
        } else if self.confirmation.is_none() {
            self.confirmation = confirmation_for_risk(self.risk, &self.subject_zones);
        }
        let local_policy_detail = if policy_mode == PermissionMode::DangerFullAccess
            && floor.local_policy_decision == ApprovalMode::Ask
        {
            "danger-full-access preserves the explicit local policy result".to_owned()
        } else {
            format!("{} local policy", floor.local_policy_decision.as_str())
        };
        self.reasons.push(PermissionDecisionReason {
            source: PermissionDecisionSource::HardSafety,
            code: "permission_effect_floor".to_owned(),
            detail: format!(
                "declared effects require at least {} risk; {local_policy_detail}",
                permission_risk_label(floor.risk),
            ),
        });
        self.recompute_mode();
    }

    fn allow_contained_workspace_validation_default(&mut self) {
        self.base_local_policy_decision = ApprovalMode::Allow;
        self.local_policy_decision = combine_modes(vec![
            self.base_local_policy_decision,
            self.external_directory_policy_decision,
        ]);
        self.reasons.push(PermissionDecisionReason {
            source: PermissionDecisionSource::SandboxSubstitution,
            code: "contained_workspace_validation_default".to_owned(),
            detail: "auto-edit allowed complete workspace validation under proven containment"
                .to_owned(),
        });
        self.recompute_mode();
    }

    pub(crate) fn request_external_directory_interactive_approval(&mut self) {
        if self.danger_full_access
            || !self.external_directory_required
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

    fn enforce_critical_path_hard_safety(&mut self) {
        let Some(target) = self.subjects.iter().find_map(|subject| {
            let target = classify_hard_safety_path_target(subject)?;
            hard_safety_target_blocks_operation(target, self.access, self.operation)
                .then_some(target)
        }) else {
            return;
        };

        self.risk = PermissionRisk::Protected;
        self.base_local_policy_decision = ApprovalMode::Deny;
        self.local_policy_decision = ApprovalMode::Deny;
        self.external_directory_required = false;
        self.confirmation = None;
        self.snapshot_required = false;
        self.reasons.push(PermissionDecisionReason {
            source: PermissionDecisionSource::HardSafety,
            code: "critical_path_circuit_breaker".to_owned(),
            detail: match target {
                HardSafetyPathTarget::FilesystemRoot => {
                    "filesystem-root mutation cannot be authorized by permission modes, approvals, or grants"
                }
                HardSafetyPathTarget::UserHome => {
                    "destructive user-home mutation cannot be authorized by permission modes, approvals, or grants"
                }
                HardSafetyPathTarget::SystemRoot => {
                    "system-path mutation cannot be authorized by permission modes, approvals, or grants"
                }
            }
            .to_owned(),
        });
        self.recompute_mode();
    }
}

/// Derives risk while honoring access attached to each concrete subject. The enclosing tool
/// access remains a conservative fallback for ordinary write tools, but an Execute shell call
/// does not turn a read-only `.git` observation into a protected mutation.
fn derive_permission_risk_with_subject_access(
    access: ToolAccess,
    network_effect: Option<NetworkEffect>,
    operation: ToolOperation,
    zones: &[PathTrustZone],
    overlays: &[PathRiskOverlay],
    subjects: &[ToolSubject],
) -> PermissionRisk {
    let operation_proves_path_mutation = matches!(
        operation,
        ToolOperation::CreateFile
            | ToolOperation::EditFile
            | ToolOperation::OverwriteFile
            | ToolOperation::DeleteFile
            | ToolOperation::RenamePath
            | ToolOperation::CreateDirectory
            | ToolOperation::DeleteDirectory
            | ToolOperation::RecursiveDelete
            | ToolOperation::ApplyChangeSet
            | ToolOperation::ExecuteMutatingCommand
            | ToolOperation::ExecuteDestructiveCommand
    );
    let path_mutates_sensitive_zone =
        subjects
            .iter()
            .zip(zones.iter().copied())
            .any(|(subject, zone)| {
                let subject_writes =
                    subject.access == ToolAccess::Write || access == ToolAccess::Write;
                subject.kind == ToolSubjectKind::Path
                    && ((matches!(
                        zone,
                        PathTrustZone::WorkspaceGitMetadata
                            | PathTrustZone::WorkspaceRuntimeState
                            | PathTrustZone::WorkspaceConfigSecret
                            | PathTrustZone::UserState
                            | PathTrustZone::UserCache
                    ) && (subject_writes || operation_proves_path_mutation))
                        || (subject_writes && overlays.contains(&PathRiskOverlay::SensitiveName)))
            });
    if path_mutates_sensitive_zone || (subjects.is_empty() && access != ToolAccess::Read) {
        return derive_permission_risk_with_network_effect(
            access,
            network_effect,
            operation,
            zones,
            overlays,
        );
    }

    if overlays.contains(&PathRiskOverlay::SensitiveName)
        && access != ToolAccess::Read
        && !matches!(
            operation,
            ToolOperation::Read
                | ToolOperation::Search
                | ToolOperation::ExecuteReadOnlyCommand
                | ToolOperation::NetworkRequest
        )
    {
        return PermissionRisk::High;
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
    if matches!(
        operation,
        ToolOperation::ResizeTerminalTask | ToolOperation::CancelTerminalTask
    ) {
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

/// Runtime-mutable permission mode consulted at decision time.
///
/// The agent loop builds the policy chain once per run; this override lets the host switch the
/// primary permission mode while the run is active without rebuilding the chain. `None` means the
/// configured mode applies.
#[derive(Debug, Clone, Default)]
pub struct PermissionModeOverride {
    inner: std::sync::Arc<std::sync::Mutex<Option<PermissionMode>>>,
}

impl PermissionModeOverride {
    /// Creates an override that starts with the configured mode (no override).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the effective primary mode for subsequent decisions.
    pub fn set(&self, mode: PermissionMode) {
        *self
            .inner
            .lock()
            .expect("permission mode override lock poisoned") = Some(mode);
    }

    /// Clears the override; the configured mode applies again.
    pub fn clear(&self) {
        *self
            .inner
            .lock()
            .expect("permission mode override lock poisoned") = None;
    }

    /// Returns the currently effective override, if any.
    #[must_use]
    pub fn current(&self) -> Option<PermissionMode> {
        *self
            .inner
            .lock()
            .expect("permission mode override lock poisoned")
    }
}

/// Policy evaluator that resolves allow/ask/deny for one tool call.
pub struct PermissionPolicy<'a> {
    config: &'a PermissionConfig,
    context: Option<&'a PermissionEvaluationContext>,
    mode_override: Option<&'a PermissionModeOverride>,
    command_patterns: Vec<CompiledCommandPermissionPattern<'a>>,
    rules: Vec<CompiledPermissionRule<'a>>,
    external_rules: Vec<CompiledExternalDirectoryRule<'a>>,
}

/// Permission evaluator for a primary run policy plus delegated narrowing constraints.
///
/// Each policy is evaluated independently for the concrete tool spec, operation, and subjects.
/// The resulting decisions are first combined monotonically. When the primary mode is
/// [`PermissionMode::DangerFullAccess`], the final decision then converts any remaining `Ask`
/// facet to `Allow`; `Deny` facets remain authoritative.
pub struct PermissionPolicyChain<'a> {
    policies: Vec<PermissionPolicy<'a>>,
    primary_mode: PermissionMode,
    mode_override: Option<&'a PermissionModeOverride>,
}

impl<'a> PermissionPolicyChain<'a> {
    /// Creates a decision-time policy chain from the primary config and runtime constraints.
    pub fn new_with_context(
        config: &'a PermissionConfig,
        context: &'a PermissionEvaluationContext,
    ) -> Self {
        Self::new_with_context_and_mode_override(config, context, None)
    }

    /// Creates a decision-time policy chain with a runtime-mutable primary mode override.
    pub fn new_with_context_and_mode_override(
        config: &'a PermissionConfig,
        context: &'a PermissionEvaluationContext,
        mode_override: Option<&'a PermissionModeOverride>,
    ) -> Self {
        let mut policies =
            Vec::with_capacity(context.delegated_policy_constraints.len().saturating_add(1));
        policies.push(PermissionPolicy::new_with_context_and_mode_override(
            config,
            Some(context),
            mode_override,
        ));
        policies.extend(
            context
                .delegated_policy_constraints
                .iter()
                .map(|constraint| {
                    PermissionPolicy::new_with_context_and_mode_override(
                        constraint,
                        Some(context),
                        None,
                    )
                }),
        );
        Self {
            policies,
            primary_mode: config.mode,
            mode_override,
        }
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
        if self.effective_primary_mode() == PermissionMode::DangerFullAccess {
            decision.danger_full_access = true;
            decision.suppress_danger_full_access_asks();
        }
        Ok(decision)
    }

    fn effective_primary_mode(&self) -> PermissionMode {
        self.mode_override
            .and_then(PermissionModeOverride::current)
            .unwrap_or(self.primary_mode)
    }

    /// Resolves a V2 plan and applies analysis safety after all delegated policies are combined.
    pub fn decide_plan(
        &self,
        spec: &ToolSpec,
        plan: &ToolPermissionPlanV2,
    ) -> Result<PermissionDecision> {
        let mut policies = self.policies.iter();
        let primary = policies
            .next()
            .expect("permission policy chain always contains the primary policy");
        let mut decision = primary.decide_plan(spec, plan)?;
        for policy in policies {
            decision.restrict_with(policy.decide_plan(spec, plan)?);
        }
        if self.primary_mode == PermissionMode::DangerFullAccess {
            decision.danger_full_access = true;
            decision.suppress_danger_full_access_asks();
        }
        Ok(decision)
    }
}

impl<'a> PermissionPolicy<'a> {
    /// Creates a policy evaluator from shared configuration.
    pub fn new(config: &'a PermissionConfig) -> Self {
        Self::new_with_context_and_mode_override(config, None, None)
    }

    /// Creates a policy evaluator with the resolved runtime path context.
    pub fn new_with_context(
        config: &'a PermissionConfig,
        context: &'a PermissionEvaluationContext,
    ) -> Self {
        Self::new_with_context_and_mode_override(config, Some(context), None)
    }

    /// Creates a policy evaluator with the resolved runtime path context and a runtime-mutable
    /// primary mode override.
    pub fn new_with_context_and_mode_override(
        config: &'a PermissionConfig,
        context: Option<&'a PermissionEvaluationContext>,
        mode_override: Option<&'a PermissionModeOverride>,
    ) -> Self {
        Self {
            config,
            context,
            mode_override,
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

    /// Returns the effective primary mode: the runtime override when present, else the configured
    /// mode.
    fn effective_mode(&self) -> PermissionMode {
        self.mode_override
            .and_then(PermissionModeOverride::current)
            .unwrap_or(self.config.mode)
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
        decision.suppress_danger_full_access_asks();
        Ok(decision)
    }

    fn decide_plan(
        &self,
        spec: &ToolSpec,
        plan: &ToolPermissionPlanV2,
    ) -> Result<PermissionDecision> {
        let mut decision = self.decide_with_operation_network_effect_and_default(
            spec,
            &plan.tool_name,
            plan.access,
            plan.operation,
            plan.network_effect(),
            plan.subjects.clone(),
            plan.tool_default_mode,
        )?;
        decision.restrict_for_permission_effects(self.config.mode, &plan.effects);
        if !plan.analysis.is_complete() {
            decision.restrict_for_incomplete_analysis(self.config.mode, &plan.analysis);
            return Ok(decision);
        }
        if spec.category != ToolCategory::Shell {
            return Ok(decision);
        }
        if plan.analysis.is_complete()
            && self.config.mode == PermissionMode::AutoEdit
            && plan.operation == ToolOperation::ExecuteWorkspaceCheckCommand
            && plan
                .effects
                .contains(&ToolPermissionEffect::ExecuteWorkspaceCode)
            && !plan.has_non_grantable_effect()
            && plan.containment.network == NetworkContainment::Deny
            && plan
                .analysis_bindings
                .get("containment_proven")
                .is_some_and(|value| value == "true")
            && decision.base_local_policy_decision == ApprovalMode::Ask
            && decision.external_directory_policy_decision == ApprovalMode::Allow
            && decision.network_policy_decision == ApprovalMode::Allow
            && decision.source_policy_decision == ApprovalMode::Allow
            && decision.confirmation.is_none()
            && !decision.snapshot_required
            && decision.command_permission_matches.is_empty()
            && !self.has_explicit_tool_policy(&plan.tool_name, &plan.subjects)?
        {
            decision.allow_contained_workspace_validation_default();
        } else {
            decision.restrict_for_incomplete_analysis(self.config.mode, &plan.analysis);
        }
        Ok(decision)
    }

    fn has_explicit_tool_policy(&self, tool_name: &str, subjects: &[ToolSubject]) -> Result<bool> {
        if subjects.is_empty() {
            return Ok(self.explicit_tool_policy_mode(tool_name, None)?.is_some());
        }
        subjects.iter().try_fold(false, |found, subject| {
            Ok(found
                || self
                    .explicit_tool_policy_mode(tool_name, Some(subject))?
                    .is_some())
        })
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
        let mut mode = self.effective_mode().baseline_for(access, operation, zone);

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
        "terminal_resize" => ToolOperation::ResizeTerminalTask,
        "terminal_cancel" => ToolOperation::CancelTerminalTask,
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
    if subject.scope == ToolSubjectScope::RuntimeScratch {
        return PathTrustAnalysis {
            zone: PathTrustZone::RuntimeScratch,
            overlays: path_risk_overlays(subject),
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
    if subject.scope == ToolSubjectScope::RuntimeScratch {
        return PathTrustAnalysis {
            zone: PathTrustZone::RuntimeScratch,
            overlays: path_risk_overlays(subject),
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

fn classify_hard_safety_path_target(subject: &ToolSubject) -> Option<HardSafetyPathTarget> {
    if subject.kind != ToolSubjectKind::Path {
        return None;
    }
    let path = normalize_policy_path(subject.canonical_path.as_deref()?);
    if path.as_os_str().is_empty() {
        return None;
    }
    if path_is_filesystem_root(&path) {
        return Some(HardSafetyPathTarget::FilesystemRoot);
    }
    if platform_critical_system_roots()
        .iter()
        .any(|root| path.starts_with(root))
    {
        return Some(HardSafetyPathTarget::SystemRoot);
    }
    if subject.scope == ToolSubjectScope::External
        && canonical_user_home_root().is_some_and(|home| path.starts_with(home))
    {
        return Some(HardSafetyPathTarget::UserHome);
    }
    None
}

fn hard_safety_target_blocks_operation(
    target: HardSafetyPathTarget,
    access: ToolAccess,
    operation: ToolOperation,
) -> bool {
    match target {
        HardSafetyPathTarget::FilesystemRoot | HardSafetyPathTarget::SystemRoot => {
            path_operation_is_mutating(access, operation)
        }
        HardSafetyPathTarget::UserHome => path_operation_is_destructive(operation),
    }
}

fn path_operation_is_mutating(access: ToolAccess, operation: ToolOperation) -> bool {
    access != ToolAccess::Read
        && matches!(
            operation,
            ToolOperation::CreateFile
                | ToolOperation::EditFile
                | ToolOperation::OverwriteFile
                | ToolOperation::DeleteFile
                | ToolOperation::RenamePath
                | ToolOperation::CreateDirectory
                | ToolOperation::DeleteDirectory
                | ToolOperation::RecursiveDelete
                | ToolOperation::ApplyChangeSet
                | ToolOperation::ExecuteMutatingCommand
                | ToolOperation::ExecuteDestructiveCommand
        )
}

fn path_operation_is_destructive(operation: ToolOperation) -> bool {
    matches!(
        operation,
        ToolOperation::OverwriteFile
            | ToolOperation::DeleteFile
            | ToolOperation::RenamePath
            | ToolOperation::DeleteDirectory
            | ToolOperation::RecursiveDelete
            | ToolOperation::ApplyChangeSet
            | ToolOperation::ExecuteMutatingCommand
            | ToolOperation::ExecuteDestructiveCommand
    )
}

fn path_is_filesystem_root(path: &Path) -> bool {
    path.has_root() && path.parent().is_none()
}

fn canonical_user_home_root() -> Option<PathBuf> {
    static USER_HOME_ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
    USER_HOME_ROOT
        .get_or_init(|| {
            home_dir()
                .ok()
                .map(|path| canonicalize_hard_safety_root(&path))
        })
        .clone()
}

fn canonicalize_hard_safety_root(path: &Path) -> PathBuf {
    std::fs::canonicalize(path)
        .map(|path| normalize_policy_path(&path))
        .unwrap_or_else(|_| normalize_policy_path(path))
}

fn platform_critical_system_roots() -> &'static [PathBuf] {
    static SYSTEM_ROOTS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    SYSTEM_ROOTS.get_or_init(|| {
        let mut roots = Vec::new();
        #[cfg(target_os = "macos")]
        roots.extend([
            PathBuf::from("/System"),
            PathBuf::from("/Library"),
            PathBuf::from("/Applications"),
            PathBuf::from("/usr"),
            PathBuf::from("/bin"),
            PathBuf::from("/sbin"),
            PathBuf::from("/private/etc"),
            PathBuf::from("/private/var/db"),
        ]);
        #[cfg(all(unix, not(target_os = "macos")))]
        roots.extend([
            PathBuf::from("/boot"),
            PathBuf::from("/dev"),
            PathBuf::from("/etc"),
            PathBuf::from("/proc"),
            PathBuf::from("/sys"),
            PathBuf::from("/usr"),
            PathBuf::from("/bin"),
            PathBuf::from("/sbin"),
            PathBuf::from("/lib"),
            PathBuf::from("/lib64"),
            PathBuf::from("/run"),
            PathBuf::from("/var/lib"),
            PathBuf::from("/var/log"),
            PathBuf::from("/var/run"),
            PathBuf::from("/var/spool"),
        ]);
        #[cfg(windows)]
        {
            roots.extend([
                PathBuf::from(r"C:\Windows"),
                PathBuf::from(r"C:\Program Files"),
                PathBuf::from(r"C:\Program Files (x86)"),
                PathBuf::from(r"C:\ProgramData"),
            ]);
            for variable in [
                "SystemRoot",
                "ProgramFiles",
                "ProgramFiles(x86)",
                "ProgramData",
            ] {
                if let Some(path) = std::env::var_os(variable).map(PathBuf::from) {
                    roots.push(path);
                }
            }
        }
        roots
            .into_iter()
            .map(|path| canonicalize_hard_safety_root(&path))
            .filter(|path| !path.as_os_str().is_empty())
            .fold(Vec::new(), |mut roots, root| {
                if !roots.contains(&root) {
                    roots.push(root);
                }
                roots
            })
    })
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
    let operation_is_read_only = matches!(
        operation,
        ToolOperation::Read
            | ToolOperation::Search
            | ToolOperation::ExecuteReadOnlyCommand
            | ToolOperation::NetworkRequest
    );
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
        && !operation_is_read_only
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

    if matches!(
        operation,
        ToolOperation::ResizeTerminalTask | ToolOperation::CancelTerminalTask
    ) {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PermissionEffectPolicyFloor {
    risk: PermissionRisk,
    local_policy_decision: ApprovalMode,
    snapshot_required: bool,
}

impl PermissionEffectPolicyFloor {
    const fn new(
        risk: PermissionRisk,
        local_policy_decision: ApprovalMode,
        snapshot_required: bool,
    ) -> Self {
        Self {
            risk,
            local_policy_decision,
            snapshot_required,
        }
    }

    fn combine(self, constraint: Self) -> Self {
        Self {
            risk: self.risk.max(constraint.risk),
            local_policy_decision: combine_modes(vec![
                self.local_policy_decision,
                constraint.local_policy_decision,
            ]),
            snapshot_required: self.snapshot_required || constraint.snapshot_required,
        }
    }
}

/// Derives a closed, monotone safety floor from exact permission-plan effects.
///
/// Every `ToolPermissionEffect` is matched explicitly so adding a new effect cannot silently fall
/// back to a permissive default. The access label only participates in coherence checks for the
/// otherwise auto-grantable write and bounded-execution effects.
fn permission_effect_policy_floor(
    effects: &BTreeSet<ToolPermissionEffect>,
    access: ToolAccess,
    operation: ToolOperation,
) -> PermissionEffectPolicyFloor {
    effects.iter().fold(
        PermissionEffectPolicyFloor::new(PermissionRisk::Low, ApprovalMode::Allow, false),
        |floor, effect| floor.combine(permission_effect_floor(*effect, access, operation)),
    )
}

fn permission_effect_floor(
    effect: ToolPermissionEffect,
    access: ToolAccess,
    operation: ToolOperation,
) -> PermissionEffectPolicyFloor {
    use ToolPermissionEffect as Effect;

    match effect {
        Effect::FileRead | Effect::AgentLifecycle => {
            PermissionEffectPolicyFloor::new(PermissionRisk::Low, ApprovalMode::Allow, false)
        }
        Effect::FileWrite => PermissionEffectPolicyFloor::new(
            PermissionRisk::Medium,
            if access == ToolAccess::Read {
                ApprovalMode::Ask
            } else {
                ApprovalMode::Allow
            },
            false,
        ),
        Effect::FileDelete => {
            PermissionEffectPolicyFloor::new(PermissionRisk::Destructive, ApprovalMode::Ask, true)
        }
        Effect::ExecuteTrustedBinary if operation == ToolOperation::ExecuteReadOnlyCommand => {
            PermissionEffectPolicyFloor::new(PermissionRisk::Low, ApprovalMode::Allow, false)
        }
        Effect::ExecuteTrustedBinary | Effect::ExecuteWorkspaceCode => {
            PermissionEffectPolicyFloor::new(
                PermissionRisk::Medium,
                if access == ToolAccess::Execute
                    || operation == ToolOperation::ExecuteWorkspaceCheckCommand
                {
                    ApprovalMode::Allow
                } else {
                    ApprovalMode::Ask
                },
                false,
            )
        }
        Effect::ProcessControl
            if matches!(
                operation,
                ToolOperation::ResizeTerminalTask | ToolOperation::CancelTerminalTask
            ) =>
        {
            // These operations are generation-bound to a Sigil-owned terminal task. They cannot
            // select an arbitrary OS process, and the terminal manager revalidates exact session
            // ownership before applying the control action.
            PermissionEffectPolicyFloor::new(PermissionRisk::Medium, ApprovalMode::Allow, false)
        }
        Effect::ExecuteDynamicCode
        | Effect::NetworkMutate
        | Effect::NetworkUnknown
        | Effect::ProcessControl
        | Effect::PrivilegeEscalation
        | Effect::PersistenceChange
        | Effect::RemoteMutation
        | Effect::ExternalApplicationControl
        | Effect::Unknown => {
            PermissionEffectPolicyFloor::new(PermissionRisk::High, ApprovalMode::Ask, false)
        }
        Effect::NetworkRead => {
            PermissionEffectPolicyFloor::new(PermissionRisk::High, ApprovalMode::Allow, false)
        }
        Effect::CredentialAccess => {
            PermissionEffectPolicyFloor::new(PermissionRisk::Protected, ApprovalMode::Deny, false)
        }
    }
}

const fn permission_risk_label(risk: PermissionRisk) -> &'static str {
    match risk {
        PermissionRisk::Low => "low",
        PermissionRisk::Medium => "medium",
        PermissionRisk::High => "high",
        PermissionRisk::Destructive => "destructive",
        PermissionRisk::Protected => "protected",
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
    .is_ok()
}

/// Returns true only when a complete V2 plan can be safely widened to bounded session authority.
pub fn tool_approval_session_grant_available_for_plan(
    decision: &PermissionDecision,
    plan: &ToolPermissionPlanV2,
) -> bool {
    tool_approval_session_grant_availability_for_plan(decision, plan).is_available()
}

/// Explains whether a complete V2 plan can become bounded session authority.
///
/// `available == true` is represented only by `unavailable_reason == None`; every unavailable
/// result contains one stable reason code selected in deterministic policy-evaluation order.
pub fn tool_approval_session_grant_availability_for_plan(
    decision: &PermissionDecision,
    plan: &ToolPermissionPlanV2,
) -> ToolApprovalSessionGrantAvailability {
    use ToolApprovalSessionGrantUnavailableReasonCode as Reason;

    if !plan.analysis.is_complete() {
        return ToolApprovalSessionGrantAvailability::unavailable(Reason::AnalysisIncomplete);
    }
    if plan.semantic_scope.is_none() {
        return ToolApprovalSessionGrantAvailability::unavailable(Reason::SemanticScopeUnavailable);
    }
    if plan.has_non_grantable_effect() {
        return ToolApprovalSessionGrantAvailability::unavailable(Reason::NonGrantableEffect);
    }
    if plan.session_grant_containment_binding().is_none() {
        return ToolApprovalSessionGrantAvailability::unavailable(
            Reason::ContainmentBindingUnavailable,
        );
    }
    match tool_approval_session_grant_shape_result(decision) {
        Ok(_) => ToolApprovalSessionGrantAvailability::available(),
        Err(code) => ToolApprovalSessionGrantAvailability::unavailable(code),
    }
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
    .is_ok()
}

pub(crate) fn tool_approval_session_grant_shape(
    decision: &PermissionDecision,
) -> Option<ToolApprovalSessionGrantShape> {
    tool_approval_session_grant_shape_result(decision).ok()
}

fn tool_approval_session_grant_shape_result(
    decision: &PermissionDecision,
) -> std::result::Result<ToolApprovalSessionGrantShape, ToolApprovalSessionGrantUnavailableReasonCode>
{
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
) -> std::result::Result<ToolApprovalSessionGrantShape, ToolApprovalSessionGrantUnavailableReasonCode>
{
    use ToolApprovalSessionGrantUnavailableReasonCode as Reason;

    if source_policy_decision != ApprovalMode::Allow
        || matches!(local_policy_decision, ApprovalMode::Deny)
        || matches!(network_policy_decision, ApprovalMode::Deny)
    {
        return Err(Reason::PolicyDecisionNotGrantable);
    }

    let mut facets = Vec::new();
    if local_policy_decision == ApprovalMode::Ask {
        facets.push(ToolApprovalSessionGrantFacet::Local);
    }
    if network_policy_decision == ApprovalMode::Ask {
        facets.push(ToolApprovalSessionGrantFacet::Network);
    }
    if facets.is_empty() {
        return Err(Reason::NoReusableApprovalFacet);
    }

    let network_read_scope = network_read_session_grant_scope_result(
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
        return Ok(ToolApprovalSessionGrantShape { facets, scope });
    }
    if let Ok(scope) = network_read_scope {
        return Ok(ToolApprovalSessionGrantShape { facets, scope });
    }

    let network_risk_is_consistent = network_effect.is_none() || risk >= PermissionRisk::High;
    if network_policy_decision != ApprovalMode::Allow || !network_risk_is_consistent {
        return Err(Reason::NetworkScopeNotGrantable);
    }
    tool_approval_session_grant_parts_result(
        access,
        operation,
        risk,
        subjects,
        zones,
        confirmation,
        snapshot_required,
    )?;
    Ok(ToolApprovalSessionGrantShape {
        facets,
        scope: command_family_grant_scope(subjects)
            .unwrap_or(ToolApprovalSessionGrantScope::ExactSubjects),
    })
}

/// Derives a `CommandFamily` grant scope for shell commands whose family is not statically
/// known. Known families already carry a `family:`-prefixed stable subject and keep the exact
/// `ExactSubjects` path so the existing family grants are unchanged. The stored prefix is the
/// plain first-two-token pair without the trailing glob `*`.
fn command_family_grant_scope(subjects: &[ToolSubject]) -> Option<ToolApprovalSessionGrantScope> {
    subjects.iter().find_map(|subject| {
        if subject.kind != ToolSubjectKind::Command || subject.normalized.starts_with("family:") {
            return None;
        }
        command_family_prefix(&subject.original)
            .map(|prefix| ToolApprovalSessionGrantScope::CommandFamily { prefix })
    })
}

/// The plain first-two-token family prefix (`cargo test`) derived from a command, or `None`
/// when the command must not be widened.
pub(crate) fn command_family_prefix(command: &str) -> Option<String> {
    derive_command_family_allow_pattern(command)
        .map(|pattern| pattern.strip_suffix('*').unwrap_or(&pattern).to_owned())
}

fn network_read_session_grant_scope_result(
    access: ToolAccess,
    network_effect: Option<NetworkEffect>,
    operation: ToolOperation,
    risk: PermissionRisk,
    subjects: &[ToolSubject],
    confirmation: Option<&PermissionConfirmation>,
    snapshot_required: bool,
) -> std::result::Result<ToolApprovalSessionGrantScope, ToolApprovalSessionGrantUnavailableReasonCode>
{
    use ToolApprovalSessionGrantUnavailableReasonCode as Reason;

    if access != ToolAccess::Read
        || network_effect != Some(NetworkEffect::Read)
        || operation != ToolOperation::NetworkRequest
        || risk != PermissionRisk::High
    {
        return Err(Reason::NetworkScopeNotGrantable);
    }
    if confirmation.is_some() {
        return Err(Reason::ConfirmationRequired);
    }
    if snapshot_required {
        return Err(Reason::SnapshotRequired);
    }
    if subjects.is_empty() {
        return Err(Reason::SubjectScopeUnavailable);
    }
    if subjects.iter().all(|subject| {
        subject.kind == ToolSubjectKind::NetworkEndpoint && !subject.normalized.trim().is_empty()
    }) {
        return Ok(ToolApprovalSessionGrantScope::NetworkReadTool);
    }
    subjects
        .iter()
        .all(stable_network_read_subject)
        .then_some(ToolApprovalSessionGrantScope::ExactSubjects)
        .ok_or(Reason::SubjectScopeUnavailable)
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
    tool_approval_session_grant_parts_result(
        access,
        operation,
        risk,
        subjects,
        zones,
        confirmation,
        snapshot_required,
    )
    .is_ok()
}

fn tool_approval_session_grant_parts_result(
    access: ToolAccess,
    operation: ToolOperation,
    risk: PermissionRisk,
    subjects: &[ToolSubject],
    zones: &[PathTrustZone],
    confirmation: Option<&PermissionConfirmation>,
    snapshot_required: bool,
) -> std::result::Result<(), ToolApprovalSessionGrantUnavailableReasonCode> {
    use ToolApprovalSessionGrantUnavailableReasonCode as Reason;

    if confirmation.is_some() {
        return Err(Reason::ConfirmationRequired);
    }
    if snapshot_required {
        return Err(Reason::SnapshotRequired);
    }
    if subjects.is_empty() {
        return Err(Reason::SubjectScopeUnavailable);
    }
    if !matches!(risk, PermissionRisk::Low | PermissionRisk::Medium) {
        return Err(Reason::RiskNotGrantable);
    }
    if zones.contains(&PathTrustZone::External) && access != ToolAccess::Read {
        return Err(Reason::ExternalMutation);
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
    ) {
        return Err(Reason::OperationNotGrantable);
    }
    if !subjects
        .iter()
        .all(|subject| subject_has_stable_session_grant_scope(subject, operation))
    {
        return Err(Reason::SubjectScopeUnavailable);
    }
    Ok(())
}

fn subject_has_stable_session_grant_scope(subject: &ToolSubject, operation: ToolOperation) -> bool {
    match subject.kind {
        ToolSubjectKind::Path => match subject.scope {
            ToolSubjectScope::Workspace | ToolSubjectScope::RuntimeScratch => {
                !subject.normalized.trim().is_empty()
            }
            ToolSubjectScope::External => subject
                .canonical_path
                .as_ref()
                .is_some_and(|path| !path.as_os_str().is_empty()),
            ToolSubjectScope::Unknown => false,
        },
        ToolSubjectKind::Command => {
            matches!(
                operation,
                ToolOperation::ExecuteReadOnlyCommand | ToolOperation::ExecuteWorkspaceCheckCommand
            ) && !subject.normalized.trim().is_empty()
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
        return if risk == PermissionRisk::Protected {
            ApprovalMode::Deny
        } else {
            mode
        };
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

/// First tokens that must never be widened into a durable allow pattern, regardless of the
/// remaining command text. `rm`/`sudo` style programs are destructive by themselves; `git`
/// mutating subcommands are rejected separately on the second token so read-only git families
/// stay derivable.
const NEVER_DERIVE_FAMILY_PROGRAMS: [&str; 22] = [
    "rm", "rmdir", "dd", "shred", "mkfs", "fdisk", "parted", "mount", "umount", "kill", "pkill",
    "killall", "chmod", "chown", "chgrp", "mv", "cp", "ln", "sudo", "tee", "truncate", "cd",
];

/// `git` subcommands that mutate remote or branch state; deriving a durable allow pattern from
/// them would silently authorize future pushes, merges, or resets.
const NEVER_DERIVE_GIT_MUTATING_SUBCOMMANDS: [&str; 11] = [
    "push",
    "pull",
    "fetch",
    "merge",
    "rebase",
    "reset",
    "clean",
    "cherry-pick",
    "revert",
    "tag",
    "switch",
];

/// Derives a conservative `program subcommand*` allow pattern for a whitespace-normalized shell
/// command, or `None` when the command must not be widened into a durable family rule.
///
/// The pattern is a prefix glob over the first two tokens (`cargo test -p x 2>&1 | tail -60` ->
/// `cargo test*`), which also matches the bare `cargo test` form. The glob semantics of
/// [`command_pattern_matches`] are prefix-based, so `cargo test*` additionally matches
/// hypothetical `cargo testfoo ...` commands; the derived pattern is always shown verbatim in
/// the approval UI and the derivation refuses destructive programs, flags as the second token,
/// shell operators, and path-like tokens.
#[must_use]
pub fn derive_command_family_allow_pattern(command: &str) -> Option<String> {
    let tokens = command.split_whitespace().collect::<Vec<_>>();
    let [program, subcommand, ..] = tokens.as_slice() else {
        return None;
    };
    if program.starts_with('-') || program.contains('/') {
        return None;
    }
    if NEVER_DERIVE_FAMILY_PROGRAMS.contains(program) {
        return None;
    }
    if subcommand.starts_with('-')
        || subcommand.starts_with('/')
        || subcommand.starts_with('.')
        || subcommand
            .chars()
            .next()
            .is_some_and(|character| matches!(character, '|' | '&' | ';' | '<' | '>'))
    {
        return None;
    }
    if *program == "git" && NEVER_DERIVE_GIT_MUTATING_SUBCOMMANDS.contains(subcommand) {
        return None;
    }
    if tokens.iter().skip(2).any(|token| {
        token.starts_with('>') || token.starts_with('<') || matches!(*token, "&&" | "||" | ";")
    }) {
        // Redirection writes and command chains hidden in the tail would be silently widened by
        // the prefix glob; `2>&1` fd-duplication and `| <read filter>` pipes stay allowed.
        return None;
    }
    Some(format!("{program} {subcommand}*"))
}

/// Token-boundary match for a derived command-family prefix against a whitespace-normalized
/// command: exact equality or the prefix followed by a space.
pub(crate) fn command_family_prefix_matches(prefix: &str, command: &str) -> bool {
    command == prefix || command.starts_with(&format!("{prefix} "))
}

/// Derives the durable command-family allow pattern for a shell-style tool call, or `None` when
/// the tool does not take a shell command or the command must not be widened. Accepts the bash
/// `{"command": "..."}` argument shape and a bare JSON string command.
#[must_use]
pub fn derive_command_family_allow_pattern_for_call(call: &crate::ToolCall) -> Option<String> {
    let command = serde_json::from_str::<serde_json::Value>(&call.args_json)
        .ok()
        .and_then(|args| match args {
            serde_json::Value::String(command) => Some(command),
            serde_json::Value::Object(fields) => fields
                .get("command")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            _ => None,
        })?;
    derive_command_family_allow_pattern(&command)
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
