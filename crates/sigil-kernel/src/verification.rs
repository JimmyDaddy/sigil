//! RFC-0003 verification contract foundation.
//!
//! This module defines provider-neutral verification state, evidence receipts, workspace snapshot
//! binding, a minimal check runner, and a deterministic readiness reducer. The runner records
//! command/check facts as durable events and projects proof through `VerificationRecorded`.

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Component, Path, PathBuf},
    process::Command,
    time::Instant,
};

use anyhow::{Context, Result, anyhow, bail};
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    DurableEventType, EventClass, EventId, ExecutionBackend, ExecutionBackendCapabilities,
    ExecutionBackendKind, ExecutionRequest, Session, SessionId, StoredEvent,
    WorkspaceMutationDetected,
    session::{ControlEntry, SessionLogEntry},
    stable_event_uuid,
};

#[cfg(test)]
#[path = "tests/verification_tests.rs"]
mod tests;

pub type ArtifactId = String;
pub type ChangesetId = String;
pub type CheckSpecId = String;
pub type EnvironmentFingerprint = String;
pub type PolicyHash = String;
pub type ReceiptId = String;
pub type SandboxDecisionId = EventId;
pub type SandboxProfileHash = String;
pub type ToolCallId = String;
pub type VerificationScopeHash = String;
pub type WorkspaceId = String;
pub type WorkspaceRevision = u64;
pub type WorkspaceSnapshotId = String;
pub type WorkspaceTrustSnapshotId = String;

/// Default verification scope hash used by sequential task readiness until RFC-0002 supplies
/// richer per-workspace revision streams.
pub const DEFAULT_TASK_VERIFICATION_SCOPE_HASH: &str = "task_step_default";
pub const MAX_WORKSPACE_SNAPSHOT_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// Execution lifecycle, intentionally independent from verification proof status.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Completed,
    Paused,
    Blocked,
    Failed,
    Cancelled,
    Interrupted,
}

impl RunStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Blocked | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }
}

/// System-computed verification status for the relevant workspace snapshot.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationVerdict {
    NotEvaluated,
    NotApplicable,
    Pending,
    Passed,
    Failed,
    Missing,
    Inconclusive,
    Stale,
    Skipped,
}

impl VerificationVerdict {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::NotEvaluated | Self::Pending)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VisibleCompletionState {
    Running,
    Paused,
    Verified,
    Completed,
    CompletedUnverified,
    FailedVerification,
    Failed,
    Cancelled,
    Interrupted,
    NeedsUser,
}

impl VisibleCompletionState {
    pub fn derive(run_status: RunStatus, verdict: VerificationVerdict) -> Self {
        match run_status {
            RunStatus::Running => Self::Running,
            RunStatus::Paused => Self::Paused,
            RunStatus::Completed => match verdict {
                VerificationVerdict::Passed => Self::Verified,
                VerificationVerdict::NotApplicable => Self::Completed,
                _ => Self::CompletedUnverified,
            },
            RunStatus::Failed if verdict == VerificationVerdict::Failed => Self::FailedVerification,
            RunStatus::Failed => Self::Failed,
            RunStatus::Blocked => Self::NeedsUser,
            RunStatus::Cancelled => Self::Cancelled,
            RunStatus::Interrupted => Self::Interrupted,
        }
    }
}

/// User-selected policy for starting verification checks without an explicit run action.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationAutoRunPolicy {
    /// Never start checks automatically; task/session surfaces still expose run/retry actions.
    #[default]
    Manual,
    /// Start only checks that are already trusted by user config, workspace trust or promotion.
    TrustedOnly,
    /// Never start checks automatically, even when all checks are trusted.
    Never,
}

impl VerificationAutoRunPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::TrustedOnly => "trusted_only",
            Self::Never => "never",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Manual => Self::TrustedOnly,
            Self::TrustedOnly => Self::Never,
            Self::Never => Self::Manual,
        }
    }

    fn most_restrictive(self, other: Self) -> Self {
        match (self, other) {
            (Self::Never, _) | (_, Self::Never) => Self::Never,
            (Self::Manual, _) | (_, Self::Manual) => Self::Manual,
            (Self::TrustedOnly, Self::TrustedOnly) => Self::TrustedOnly,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct VerificationPolicy {
    #[serde(default)]
    pub required_checks: Vec<CheckSpec>,
    pub completion_criteria: CompletionCriteria,
    pub verification_scope: VerificationScope,
    pub sandbox_profile: SandboxProfileRequirement,
    pub workspace_trust_requirement: WorkspaceTrustRequirement,
    pub allow_unverified_completion: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub auto_run: VerificationAutoRunPolicy,
}

impl VerificationPolicy {
    pub fn no_checks_required(scope_hash: impl Into<String>) -> Self {
        Self {
            required_checks: Vec::new(),
            completion_criteria: CompletionCriteria::NoChecksRequired,
            verification_scope: VerificationScope::all_tracked(scope_hash),
            sandbox_profile: SandboxProfileRequirement::None,
            workspace_trust_requirement: WorkspaceTrustRequirement::None,
            allow_unverified_completion: true,
            timeout_ms: None,
            auto_run: VerificationAutoRunPolicy::Manual,
        }
    }

    /// Computes a deterministic content hash for persisted policy receipts.
    ///
    /// # Errors
    ///
    /// Returns an error if the policy cannot be converted to canonical JSON.
    pub fn stable_hash(&self) -> Result<PolicyHash> {
        let value = serde_json::to_value(self)
            .map_err(|error| anyhow!("failed to convert verification policy to json: {error}"))?;
        let digest = Sha256::digest(canonical_json_bytes(&value)?);
        Ok(format!("sha256:jcs-v1:{digest:x}"))
    }

    /// Merges a child policy into this parent policy.
    ///
    /// # Errors
    ///
    /// Returns an error when the child attempts to relax parent-required checks, scope, sandbox,
    /// trust, timeout or unverified-completion constraints.
    pub fn merge_child(&self, child: &Self) -> Result<Self> {
        if !child.verification_scope.covers(&self.verification_scope) {
            bail!("child verification scope does not cover parent-required scope");
        }
        ensure_no_conflicting_check_ids(&self.required_checks, &child.required_checks)?;
        if self.allow_unverified_completion && !child.allow_unverified_completion {
            // This is a valid tightening; handled below with boolean AND.
        }
        let required_checks = union_checks(&self.required_checks, &child.required_checks);
        Ok(Self {
            required_checks,
            completion_criteria: self.completion_criteria.stricter(child.completion_criteria),
            verification_scope: child.verification_scope.clone(),
            sandbox_profile: self.sandbox_profile.stricter(child.sandbox_profile)?,
            workspace_trust_requirement: self
                .workspace_trust_requirement
                .stricter(child.workspace_trust_requirement),
            allow_unverified_completion: self.allow_unverified_completion
                && child.allow_unverified_completion,
            timeout_ms: min_optional_timeout(self.timeout_ms, child.timeout_ms),
            auto_run: self.auto_run.most_restrictive(child.auto_run),
        })
    }
}

fn union_checks(parent: &[CheckSpec], child: &[CheckSpec]) -> Vec<CheckSpec> {
    let mut checks = parent.to_vec();
    for check in child {
        if let Some(existing) = checks
            .iter()
            .find(|existing| existing.check_spec_id == check.check_spec_id)
        {
            if existing.check_spec_hash != check.check_spec_hash {
                continue;
            }
        } else {
            checks.push(check.clone());
        }
    }
    checks
}

fn ensure_no_conflicting_check_ids(parent: &[CheckSpec], child: &[CheckSpec]) -> Result<()> {
    for parent_check in parent {
        if let Some(child_check) = child
            .iter()
            .find(|child_check| child_check.check_spec_id == parent_check.check_spec_id)
            && child_check.check_spec_hash != parent_check.check_spec_hash
        {
            bail!(
                "child verification policy redefines required check {}",
                parent_check.check_spec_id
            );
        }
    }
    Ok(())
}

fn min_optional_timeout(parent: Option<u64>, child: Option<u64>) -> Option<u64> {
    match (parent, child) {
        (Some(parent), Some(child)) => Some(parent.min(child)),
        (Some(parent), None) => Some(parent),
        (None, Some(child)) => Some(child),
        (None, None) => None,
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum CompletionCriteria {
    NoChecksRequired,
    AnyRequiredCheck,
    AllRequiredChecks,
}

impl CompletionCriteria {
    pub fn stricter(self, other: Self) -> Self {
        self.max(other)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct VerificationScope {
    pub scope_hash: VerificationScopeHash,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    pub tracked_files_only: bool,
    #[serde(
        default = "default_max_snapshot_file_bytes",
        skip_serializing_if = "is_default_max_snapshot_file_bytes"
    )]
    pub max_file_bytes: u64,
    #[serde(default)]
    pub generated_roots: Vec<PathBuf>,
}

impl VerificationScope {
    pub fn all_tracked(scope_hash: impl Into<String>) -> Self {
        Self::profiled(scope_hash, VerificationScopeProfile::Auto)
    }

    pub fn profiled(scope_hash: impl Into<String>, profile: VerificationScopeProfile) -> Self {
        Self {
            scope_hash: scope_hash.into(),
            include: Vec::new(),
            exclude: profile.default_excludes(),
            tracked_files_only: true,
            max_file_bytes: MAX_WORKSPACE_SNAPSHOT_FILE_BYTES,
            generated_roots: profile.generated_roots(),
        }
    }

    /// Returns true when this scope covers every path the required scope can verify.
    pub fn covers(&self, required: &Self) -> bool {
        if required.include.is_empty() && !self.include.is_empty() {
            return false;
        }
        let include_covers = required.include.iter().all(|pattern| {
            self.include.is_empty()
                || self.include.iter().any(|candidate| candidate == pattern)
                || self.include.iter().any(|candidate| candidate == "**/*")
        });
        let exclude_covers = self
            .exclude
            .iter()
            .all(|pattern| required.exclude.iter().any(|required| required == pattern));
        let tracking_covers = !self.tracked_files_only || required.tracked_files_only;
        let max_file_bytes_covers = self.max_file_bytes >= required.max_file_bytes;
        include_covers && exclude_covers && tracking_covers && max_file_bytes_covers
    }
}

/// Coarse verification-scope presets for common project layouts.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationScopeProfile {
    #[default]
    Auto,
    Rust,
    Node,
    Python,
    Docs,
}

impl VerificationScopeProfile {
    pub const ALL: [Self; 5] = [Self::Auto, Self::Rust, Self::Node, Self::Python, Self::Docs];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Rust => "rust",
            Self::Node => "node",
            Self::Python => "python",
            Self::Docs => "docs",
        }
    }

    #[must_use]
    pub fn default_excludes(self) -> Vec<String> {
        let mut excludes = default_scope_excludes();
        match self {
            Self::Auto | Self::Rust => {}
            Self::Node => {
                extend_unique(&mut excludes, &[".next/**", ".nuxt/**", ".turbo/**"]);
            }
            Self::Python => {
                extend_unique(
                    &mut excludes,
                    &[
                        "__pycache__/**",
                        ".mypy_cache/**",
                        ".ruff_cache/**",
                        ".venv/**",
                        "venv/**",
                    ],
                );
            }
            Self::Docs => {
                extend_unique(&mut excludes, &["site/**", ".docusaurus/**"]);
            }
        }
        excludes
    }

    #[must_use]
    pub fn generated_roots(self) -> Vec<PathBuf> {
        match self {
            Self::Auto | Self::Rust => Vec::new(),
            Self::Node => vec![PathBuf::from(".next"), PathBuf::from(".nuxt")],
            Self::Python => vec![
                PathBuf::from("__pycache__"),
                PathBuf::from(".mypy_cache"),
                PathBuf::from(".ruff_cache"),
            ],
            Self::Docs => vec![PathBuf::from("site"), PathBuf::from(".docusaurus")],
        }
    }

    #[must_use]
    pub fn summary(self) -> &'static str {
        match self {
            Self::Auto => "recommended build/cache excludes",
            Self::Rust => "Rust target cache excludes",
            Self::Node => "Node package/cache/build excludes",
            Self::Python => "Python cache/venv excludes",
            Self::Docs => "docs site output excludes",
        }
    }
}

pub fn default_max_snapshot_file_bytes() -> u64 {
    MAX_WORKSPACE_SNAPSHOT_FILE_BYTES
}

fn is_default_max_snapshot_file_bytes(value: &u64) -> bool {
    *value == default_max_snapshot_file_bytes()
}

pub fn default_scope_excludes() -> Vec<String> {
    [
        ".git/**",
        ".sigil/sessions/**",
        ".sigil/tasks/**",
        ".sigil/terminal/**",
        ".sigil/cache/**",
        ".sigil/artifacts/**",
        ".sigil/tmp/**",
        ".sigil/input-history.jsonl",
        ".sigil-state/**",
        ".sigil-recovery/**",
        "target/**",
        "node_modules/**",
        "dist/**",
        "coverage/**",
        ".next/**",
        ".nuxt/**",
        ".turbo/**",
        ".pytest_cache/**",
        ".mypy_cache/**",
        ".ruff_cache/**",
        "__pycache__/**",
        ".venv/**",
        "venv/**",
        ".env",
        ".env.*",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn extend_unique(values: &mut Vec<String>, extra: &[&str]) {
    for value in extra {
        if !values.iter().any(|existing| existing == value) {
            values.push((*value).to_owned());
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SandboxProfileRequirement {
    None,
    ApprovalOrSandbox,
    Sandboxed,
}

impl SandboxProfileRequirement {
    pub fn stricter(self, other: Self) -> Result<Self> {
        Ok(self.max(other))
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceTrustRequirement {
    None,
    ApprovalOrSandbox,
    Trusted,
}

impl WorkspaceTrustRequirement {
    pub fn stricter(self, other: Self) -> Self {
        self.max(other)
    }

    pub fn is_satisfied(
        self,
        trust: WorkspaceTrust,
        approval_event_id: Option<&EventId>,
        sandbox_decision_id: Option<&EventId>,
    ) -> bool {
        match self {
            Self::None => true,
            Self::ApprovalOrSandbox => {
                trust == WorkspaceTrust::Trusted
                    || approval_event_id.is_some()
                    || sandbox_decision_id.is_some()
            }
            Self::Trusted => trust == WorkspaceTrust::Trusted,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceTrust {
    Unknown,
    Trusted,
    Restricted,
    Denied,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CheckCommand {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
}

impl CheckCommand {
    pub fn shell(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            cwd: None,
        }
    }
}

/// User-level verification configuration loaded from `sigil.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct VerificationConfig {
    #[serde(default)]
    pub auto_run: VerificationAutoRunPolicy,
    #[serde(default)]
    pub scope_profile: VerificationScopeProfile,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_scope_excludes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub generated_roots: Vec<PathBuf>,
    #[serde(default)]
    pub checks: Vec<VerificationCheckConfig>,
}

impl VerificationConfig {
    /// Returns true when no user-level verification behavior is configured.
    pub fn is_empty(&self) -> bool {
        self.auto_run == VerificationAutoRunPolicy::Manual
            && self.scope_profile == VerificationScopeProfile::Auto
            && self.extra_scope_excludes.is_empty()
            && self.generated_roots.is_empty()
            && self.checks.is_empty()
    }

    #[must_use]
    pub fn scope_for_hash(
        &self,
        scope_hash: impl Into<VerificationScopeHash>,
    ) -> VerificationScope {
        let mut scope = VerificationScope::profiled(scope_hash, self.scope_profile);
        extend_unique(
            &mut scope.exclude,
            &self
                .extra_scope_excludes
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        );
        for root in &self.generated_roots {
            if !scope
                .generated_roots
                .iter()
                .any(|existing| existing == root)
            {
                scope.generated_roots.push(root.clone());
            }
        }
        scope
    }
}

/// One user-configured verification check.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct VerificationCheckConfig {
    pub id: CheckSpecId,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default = "default_tool_effect_read_only")]
    pub effect: ToolEffect,
}

impl VerificationCheckConfig {
    /// Returns a normalized command validated against the workspace boundary.
    ///
    /// # Errors
    ///
    /// Returns an error when `cwd` is absolute, escapes with `..`, or resolves outside the
    /// canonical workspace root.
    pub fn normalized_command(&self, workspace_root: &Path) -> Result<CheckCommand> {
        Ok(CheckCommand {
            command: self.command.clone(),
            args: self.args.clone(),
            cwd: normalize_check_cwd(workspace_root, self.cwd.as_ref())?,
        })
    }
}

fn default_tool_effect_read_only() -> ToolEffect {
    ToolEffect::ReadOnly
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CheckSpec {
    pub check_spec_id: CheckSpecId,
    pub command: CheckCommand,
    pub effect: ToolEffect,
    pub check_spec_hash: String,
    pub verification_scope_hash: VerificationScopeHash,
}

impl CheckSpec {
    pub fn new(
        check_spec_id: impl Into<String>,
        command: CheckCommand,
        effect: ToolEffect,
        verification_scope_hash: impl Into<String>,
    ) -> Self {
        let check_spec_id = check_spec_id.into();
        let verification_scope_hash = verification_scope_hash.into();
        let command_cwd = command
            .cwd
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let check_spec_hash = stable_hash_parts(
            check_spec_id.as_str(),
            &command.command,
            command.args.iter().map(String::as_str),
            command_cwd.as_str(),
            &verification_scope_hash,
            effect.as_str(),
        );
        Self {
            check_spec_id,
            command,
            effect,
            check_spec_hash,
            verification_scope_hash,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CandidateCheck {
    pub source: CheckDiscoverySource,
    pub command: CheckCommand,
    pub source_event_id: EventId,
    pub workspace_trust_snapshot_id: WorkspaceTrustSnapshotId,
}

impl CandidateCheck {
    pub fn promote(
        self,
        check_spec_id: impl Into<String>,
        verification_scope_hash: impl Into<String>,
        effect: ToolEffect,
        promotion: CheckPromotion,
    ) -> Result<TrustedCheckSpec> {
        if self.source.requires_trust_promotion()
            && !matches!(
                promotion,
                CheckPromotion::UserApproved { .. }
                    | CheckPromotion::Sandboxed { .. }
                    | CheckPromotion::GlobalPolicy { .. }
            )
        {
            bail!("untrusted workspace check requires approval, sandbox, or global policy");
        }
        let approval_event_id = promotion.approval_event_id();
        let sandbox_decision_id = promotion.sandbox_decision_id();
        let check_spec =
            CheckSpec::new(check_spec_id, self.command, effect, verification_scope_hash);
        Ok(TrustedCheckSpec {
            check_spec,
            source: self.source,
            workspace_trust_snapshot_id: self.workspace_trust_snapshot_id,
            promoted_by: promotion,
            approval_event_id,
            sandbox_decision_id,
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckDiscoverySource {
    SigilVerificationFile,
    UserExplicitConfig,
    CiConfig,
    PackageScript,
    Cargo,
    Makefile,
    ModelSuggested,
    UserConfirmed,
}

impl CheckDiscoverySource {
    pub fn requires_trust_promotion(self) -> bool {
        matches!(
            self,
            Self::SigilVerificationFile
                | Self::CiConfig
                | Self::PackageScript
                | Self::Cargo
                | Self::Makefile
                | Self::ModelSuggested
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CheckPromotion {
    UserApproved { approval_event_id: EventId },
    WorkspaceTrusted { trust_event_id: EventId },
    Sandboxed { sandbox_decision_id: EventId },
    GlobalPolicy { policy_event_id: EventId },
    ExplicitUserConfig { config_event_id: EventId },
}

impl CheckPromotion {
    fn approval_event_id(&self) -> Option<EventId> {
        match self {
            Self::UserApproved { approval_event_id } => Some(approval_event_id.clone()),
            Self::WorkspaceTrusted { .. }
            | Self::Sandboxed { .. }
            | Self::GlobalPolicy { .. }
            | Self::ExplicitUserConfig { .. } => None,
        }
    }

    fn sandbox_decision_id(&self) -> Option<EventId> {
        match self {
            Self::Sandboxed {
                sandbox_decision_id,
            } => Some(sandbox_decision_id.clone()),
            Self::UserApproved { .. }
            | Self::WorkspaceTrusted { .. }
            | Self::GlobalPolicy { .. }
            | Self::ExplicitUserConfig { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TrustedCheckSpec {
    pub check_spec: CheckSpec,
    #[serde(default = "default_check_discovery_source")]
    pub source: CheckDiscoverySource,
    #[serde(default)]
    pub workspace_trust_snapshot_id: WorkspaceTrustSnapshotId,
    pub promoted_by: CheckPromotion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_event_id: Option<EventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_decision_id: Option<EventId>,
}

fn default_check_discovery_source() -> CheckDiscoverySource {
    CheckDiscoverySource::UserConfirmed
}

/// Candidate check plus the stable defaults discovered for a verification policy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct DiscoveredCheck {
    pub candidate: CandidateCheck,
    pub suggested_check_spec_id: CheckSpecId,
    pub effect: ToolEffect,
    pub source_path: PathBuf,
}

impl DiscoveredCheck {
    /// Promotes a discovered candidate using the RFC-0003 workspace trust gate.
    ///
    /// # Errors
    ///
    /// Returns an error when an untrusted repository source is promoted without explicit approval,
    /// a sandbox decision or a global policy.
    pub fn promote(
        self,
        verification_scope_hash: impl Into<String>,
        promotion: CheckPromotion,
    ) -> Result<TrustedCheckSpec> {
        self.candidate.promote(
            self.suggested_check_spec_id,
            verification_scope_hash,
            self.effect,
            promotion,
        )
    }
}

/// Durable control entry recording one trusted check spec selected for a policy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CheckSpecRecordedEntry {
    pub scope: EvidenceScope,
    pub trusted_check: TrustedCheckSpec,
    pub source_event_id: EventId,
}

impl CheckSpecRecordedEntry {
    /// Builds a check-spec entry from a promoted candidate.
    pub fn new(
        scope: EvidenceScope,
        trusted_check: TrustedCheckSpec,
        source_event_id: impl Into<EventId>,
    ) -> Self {
        Self {
            scope,
            trusted_check,
            source_event_id: source_event_id.into(),
        }
    }
}

/// Builds trusted check-spec entries from explicit user configuration.
///
/// These checks are user-approved by being present in the selected user config; repository-local
/// discovered checks still remain candidates until a separate approval, sandbox or global policy
/// promotes them.
///
/// # Errors
///
/// Returns an error when the workspace root cannot be canonicalized, a check is malformed, or a
/// configured working directory escapes the workspace.
pub fn check_specs_from_user_config(
    workspace_root: impl AsRef<Path>,
    user_config: &VerificationConfig,
    scope: EvidenceScope,
    verification_scope_hash: impl Into<VerificationScopeHash>,
    config_event_id: impl Into<EventId>,
) -> Result<Vec<CheckSpecRecordedEntry>> {
    let workspace_root = workspace_root.as_ref();
    let canonical_root = fs::canonicalize(workspace_root)
        .with_context(|| format!("failed to canonicalize {}", workspace_root.display()))?;
    let verification_scope_hash = verification_scope_hash.into();
    let config_event_id = config_event_id.into();
    let mut entries = Vec::new();
    let mut used_ids = BTreeSet::new();
    for check in &user_config.checks {
        if check.id.trim().is_empty() || check.command.trim().is_empty() {
            bail!("verification user config contains a check with empty id or command");
        }
        let check_spec_id = unique_check_id(check.id.clone(), &mut used_ids);
        let command = check.normalized_command(&canonical_root)?;
        let candidate = CandidateCheck {
            source: CheckDiscoverySource::UserExplicitConfig,
            command,
            source_event_id: config_event_id.clone(),
            workspace_trust_snapshot_id: "user-config".to_owned(),
        };
        let trusted = candidate.promote(
            check_spec_id,
            verification_scope_hash.clone(),
            check.effect,
            CheckPromotion::ExplicitUserConfig {
                config_event_id: config_event_id.clone(),
            },
        )?;
        entries.push(CheckSpecRecordedEntry::new(
            scope.clone(),
            trusted,
            config_event_id.clone(),
        ));
    }
    Ok(entries)
}

/// Discovers verification check candidates from repository configuration without executing them.
///
/// # Errors
///
/// Returns an error when the workspace root cannot be canonicalized or when an explicitly present
/// verification/package manifest is malformed.
pub fn discover_candidate_checks(
    workspace_root: impl AsRef<Path>,
    workspace_trust_snapshot_id: impl Into<WorkspaceTrustSnapshotId>,
    source_event_id: impl Into<EventId>,
) -> Result<Vec<DiscoveredCheck>> {
    discover_candidate_checks_with_user_config(
        workspace_root,
        workspace_trust_snapshot_id,
        source_event_id,
        &VerificationConfig::default(),
    )
}

/// Discovers verification check candidates and includes user-configured checks before repo checks.
///
/// # Errors
///
/// Returns an error when the workspace root cannot be canonicalized, a configured check is invalid,
/// or an explicitly present repository manifest is malformed.
pub fn discover_candidate_checks_with_user_config(
    workspace_root: impl AsRef<Path>,
    workspace_trust_snapshot_id: impl Into<WorkspaceTrustSnapshotId>,
    source_event_id: impl Into<EventId>,
    user_config: &VerificationConfig,
) -> Result<Vec<DiscoveredCheck>> {
    let workspace_root = workspace_root.as_ref();
    let canonical_root = fs::canonicalize(workspace_root)
        .with_context(|| format!("failed to canonicalize {}", workspace_root.display()))?;
    let workspace_trust_snapshot_id = workspace_trust_snapshot_id.into();
    let source_event_id = source_event_id.into();
    let mut checks = Vec::new();
    let mut used_ids = BTreeSet::new();

    discover_sigil_verification_file(
        &canonical_root,
        &workspace_trust_snapshot_id,
        &source_event_id,
        &mut checks,
        &mut used_ids,
    )?;
    discover_user_config_checks(
        &canonical_root,
        user_config,
        &workspace_trust_snapshot_id,
        &source_event_id,
        &mut checks,
        &mut used_ids,
    )?;
    discover_ci_checks(
        &canonical_root,
        &workspace_trust_snapshot_id,
        &source_event_id,
        &mut checks,
        &mut used_ids,
    )?;
    discover_package_json_checks(
        &canonical_root,
        &workspace_trust_snapshot_id,
        &source_event_id,
        &mut checks,
        &mut used_ids,
    )?;
    discover_cargo_checks(
        &canonical_root,
        &workspace_trust_snapshot_id,
        &source_event_id,
        &mut checks,
        &mut used_ids,
    )?;
    discover_make_checks(
        &canonical_root,
        &workspace_trust_snapshot_id,
        &source_event_id,
        &mut checks,
        &mut used_ids,
    )?;

    Ok(checks)
}

fn discover_user_config_checks(
    workspace_root: &Path,
    user_config: &VerificationConfig,
    workspace_trust_snapshot_id: &WorkspaceTrustSnapshotId,
    source_event_id: &EventId,
    checks: &mut Vec<DiscoveredCheck>,
    used_ids: &mut BTreeSet<String>,
) -> Result<()> {
    for check in &user_config.checks {
        if check.id.trim().is_empty() || check.command.trim().is_empty() {
            bail!("verification user config contains a check with empty id or command");
        }
        let command = check.normalized_command(workspace_root)?;
        push_discovered_check(
            checks,
            used_ids,
            CheckDiscoverySource::UserExplicitConfig,
            command,
            check.id.clone(),
            check.effect,
            PathBuf::from("user-config:sigil.toml"),
            workspace_trust_snapshot_id,
            source_event_id,
        );
    }
    Ok(())
}

fn push_discovered_check(
    checks: &mut Vec<DiscoveredCheck>,
    used_ids: &mut BTreeSet<String>,
    source: CheckDiscoverySource,
    command: CheckCommand,
    suggested_check_spec_id: impl Into<String>,
    effect: ToolEffect,
    source_path: PathBuf,
    workspace_trust_snapshot_id: &WorkspaceTrustSnapshotId,
    source_event_id: &EventId,
) {
    let suggested_check_spec_id = unique_check_id(suggested_check_spec_id.into(), used_ids);
    checks.push(DiscoveredCheck {
        candidate: CandidateCheck {
            source,
            command,
            source_event_id: source_event_id.clone(),
            workspace_trust_snapshot_id: workspace_trust_snapshot_id.clone(),
        },
        suggested_check_spec_id,
        effect,
        source_path,
    });
}

fn unique_check_id(mut id: String, used_ids: &mut BTreeSet<String>) -> String {
    if used_ids.insert(id.clone()) {
        return id;
    }
    let base = id.clone();
    let mut index = 2;
    loop {
        id = format!("{base}-{index}");
        if used_ids.insert(id.clone()) {
            return id;
        }
        index += 1;
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct VerificationFile {
    #[serde(default)]
    checks: Vec<VerificationFileCheck>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct VerificationFileCheck {
    id: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    cwd: Option<PathBuf>,
    #[serde(default)]
    effect: Option<ToolEffect>,
}

fn discover_sigil_verification_file(
    workspace_root: &Path,
    workspace_trust_snapshot_id: &WorkspaceTrustSnapshotId,
    source_event_id: &EventId,
    checks: &mut Vec<DiscoveredCheck>,
    used_ids: &mut BTreeSet<String>,
) -> Result<()> {
    let path = workspace_root.join(".sigil/verification.toml");
    if !path.exists() {
        return Ok(());
    }
    let raw =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let parsed = toml::from_str::<VerificationFile>(&raw)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    for check in parsed.checks {
        if check.id.trim().is_empty() || check.command.trim().is_empty() {
            bail!(
                "{} contains a verification check with empty id or command",
                path.display()
            );
        }
        push_discovered_check(
            checks,
            used_ids,
            CheckDiscoverySource::SigilVerificationFile,
            CheckCommand {
                command: check.command,
                args: check.args,
                cwd: normalize_check_cwd(workspace_root, check.cwd.as_ref())?,
            },
            check.id,
            check.effect.unwrap_or(ToolEffect::ReadOnly),
            relative_source_path(workspace_root, &path),
            workspace_trust_snapshot_id,
            source_event_id,
        );
    }
    Ok(())
}

fn discover_ci_checks(
    workspace_root: &Path,
    workspace_trust_snapshot_id: &WorkspaceTrustSnapshotId,
    source_event_id: &EventId,
    checks: &mut Vec<DiscoveredCheck>,
    used_ids: &mut BTreeSet<String>,
) -> Result<()> {
    let workflows = workspace_root.join(".github/workflows");
    let Ok(entries) = fs::read_dir(&workflows) else {
        return Ok(());
    };
    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| matches!(extension, "yml" | "yaml"))
        })
        .collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let source_path = relative_source_path(workspace_root, &path);
        if ci_file_contains_run_command(&raw, "cargo test") {
            push_discovered_check(
                checks,
                used_ids,
                CheckDiscoverySource::CiConfig,
                CheckCommand::shell("cargo test"),
                "cargo-test-ci",
                ToolEffect::ReadOnly,
                source_path.clone(),
                workspace_trust_snapshot_id,
                source_event_id,
            );
        }
        if ci_file_contains_run_command(&raw, "npm test") {
            push_discovered_check(
                checks,
                used_ids,
                CheckDiscoverySource::CiConfig,
                CheckCommand::shell("npm test"),
                "npm-test-ci",
                ToolEffect::ReadOnly,
                source_path.clone(),
                workspace_trust_snapshot_id,
                source_event_id,
            );
        }
        if ci_file_contains_run_command(&raw, "make test") {
            push_discovered_check(
                checks,
                used_ids,
                CheckDiscoverySource::CiConfig,
                CheckCommand::shell("make test"),
                "make-test-ci",
                ToolEffect::ReadOnly,
                source_path,
                workspace_trust_snapshot_id,
                source_event_id,
            );
        }
    }
    Ok(())
}

fn ci_file_contains_run_command(raw: &str, command: &str) -> bool {
    raw.lines().any(|line| {
        let trimmed = line
            .trim_start()
            .strip_prefix("- ")
            .unwrap_or_else(|| line.trim_start())
            .trim_start();
        let Some(rest) = trimmed.strip_prefix("run:") else {
            return false;
        };
        let command_text = rest
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .trim_start_matches('|')
            .trim_start_matches('>')
            .trim();
        command_text == command || command_text.starts_with(&format!("{command} "))
    })
}

fn discover_package_json_checks(
    workspace_root: &Path,
    workspace_trust_snapshot_id: &WorkspaceTrustSnapshotId,
    source_event_id: &EventId,
    checks: &mut Vec<DiscoveredCheck>,
    used_ids: &mut BTreeSet<String>,
) -> Result<()> {
    let path = workspace_root.join("package.json");
    if !path.exists() {
        return Ok(());
    }
    let raw =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let value = serde_json::from_str::<serde_json::Value>(&raw)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    let Some(scripts) = value.get("scripts").and_then(serde_json::Value::as_object) else {
        return Ok(());
    };
    for script in ["test", "check", "lint", "build"] {
        if scripts
            .get(script)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
        {
            push_discovered_check(
                checks,
                used_ids,
                CheckDiscoverySource::PackageScript,
                CheckCommand {
                    command: "npm".to_owned(),
                    args: vec!["run".to_owned(), script.to_owned()],
                    cwd: None,
                },
                format!("npm-{script}"),
                ToolEffect::ReadOnly,
                relative_source_path(workspace_root, &path),
                workspace_trust_snapshot_id,
                source_event_id,
            );
        }
    }
    Ok(())
}

fn discover_cargo_checks(
    workspace_root: &Path,
    workspace_trust_snapshot_id: &WorkspaceTrustSnapshotId,
    source_event_id: &EventId,
    checks: &mut Vec<DiscoveredCheck>,
    used_ids: &mut BTreeSet<String>,
) -> Result<()> {
    let path = workspace_root.join("Cargo.toml");
    if !path.exists() {
        return Ok(());
    }
    let raw =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let value = toml::from_str::<toml::Value>(&raw)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    let is_workspace = value.get("workspace").is_some();
    let args = if is_workspace {
        vec!["test".to_owned(), "--workspace".to_owned()]
    } else {
        vec!["test".to_owned()]
    };
    push_discovered_check(
        checks,
        used_ids,
        CheckDiscoverySource::Cargo,
        CheckCommand {
            command: "cargo".to_owned(),
            args,
            cwd: None,
        },
        if is_workspace {
            "cargo-test-workspace"
        } else {
            "cargo-test"
        },
        ToolEffect::ReadOnly,
        relative_source_path(workspace_root, &path),
        workspace_trust_snapshot_id,
        source_event_id,
    );
    Ok(())
}

fn discover_make_checks(
    workspace_root: &Path,
    workspace_trust_snapshot_id: &WorkspaceTrustSnapshotId,
    source_event_id: &EventId,
    checks: &mut Vec<DiscoveredCheck>,
    used_ids: &mut BTreeSet<String>,
) -> Result<()> {
    let path = workspace_root.join("Makefile");
    if !path.exists() {
        return Ok(());
    }
    let raw =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    if makefile_has_target(&raw, "test") {
        push_discovered_check(
            checks,
            used_ids,
            CheckDiscoverySource::Makefile,
            CheckCommand {
                command: "make".to_owned(),
                args: vec!["test".to_owned()],
                cwd: None,
            },
            "make-test",
            ToolEffect::ReadOnly,
            relative_source_path(workspace_root, &path),
            workspace_trust_snapshot_id,
            source_event_id,
        );
    }
    Ok(())
}

fn makefile_has_target(raw: &str, target: &str) -> bool {
    raw.lines().any(|line| {
        let trimmed = line.trim_end();
        !trimmed.starts_with('\t')
            && !trimmed.starts_with('#')
            && trimmed
                .split_once(':')
                .is_some_and(|(left, _)| left.split_whitespace().any(|name| name == target))
    })
}

fn relative_source_path(workspace_root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(workspace_root)
        .unwrap_or(path)
        .to_path_buf()
}

fn normalize_check_cwd(workspace_root: &Path, cwd: Option<&PathBuf>) -> Result<Option<PathBuf>> {
    let Some(cwd) = cwd else {
        return Ok(None);
    };
    if cwd.as_os_str().is_empty() {
        return Ok(None);
    }
    if cwd.is_absolute() {
        bail!(
            "verification check cwd must be workspace-relative: {}",
            cwd.display()
        );
    }
    let mut normalized = PathBuf::new();
    for component in cwd.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                bail!(
                    "verification check cwd must not contain parent components: {}",
                    cwd.display()
                );
            }
            Component::Prefix(_) | Component::RootDir => {
                bail!(
                    "verification check cwd must be workspace-relative: {}",
                    cwd.display()
                );
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Ok(None);
    }
    let candidate = workspace_root.join(&normalized);
    if let Ok(canonical) = fs::canonicalize(&candidate)
        && !canonical.starts_with(workspace_root)
    {
        bail!(
            "verification check cwd resolves outside workspace: {}",
            cwd.display()
        );
    }
    Ok(Some(normalized))
}

/// Derives the stable workspace id used by verification snapshots for a workspace root.
///
/// # Errors
///
/// Returns an error when the workspace root cannot be canonicalized.
pub fn stable_workspace_id(workspace_root: impl AsRef<Path>) -> Result<WorkspaceId> {
    let workspace_root = workspace_root.as_ref();
    let canonical = fs::canonicalize(workspace_root)
        .with_context(|| format!("failed to canonicalize {}", workspace_root.display()))?;
    let digest = Sha256::digest(canonical.to_string_lossy().as_bytes());
    Ok(format!("workspace:{digest:x}"))
}

/// Durable control entry recording a verification policy update.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct VerificationPolicyChangedEntry {
    pub scope: EvidenceScope,
    pub policy: VerificationPolicy,
    pub policy_hash: PolicyHash,
    pub source_event_id: EventId,
}

impl VerificationPolicyChangedEntry {
    /// Builds a policy entry and computes its content hash.
    ///
    /// # Errors
    ///
    /// Returns an error if the policy cannot be hashed deterministically.
    pub fn new(
        scope: EvidenceScope,
        policy: VerificationPolicy,
        source_event_id: impl Into<EventId>,
    ) -> Result<Self> {
        let policy_hash = policy.stable_hash()?;
        Ok(Self {
            scope,
            policy,
            policy_hash,
            source_event_id: source_event_id.into(),
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolEffect {
    ReadOnly,
    WorkspaceWrite,
    ExternalWrite,
    Network,
    Unknown,
}

impl ToolEffect {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::WorkspaceWrite => "workspace_write",
            Self::ExternalWrite => "external_write",
            Self::Network => "network",
            Self::Unknown => "unknown",
        }
    }

    pub fn may_mutate_workspace(self) -> bool {
        matches!(
            self,
            Self::WorkspaceWrite | Self::ExternalWrite | Self::Unknown
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state", content = "revision")]
pub enum WorkspaceKnowledge {
    Clean(WorkspaceRevision),
    Dirty(WorkspaceRevision),
    UnknownDirty,
}

impl WorkspaceKnowledge {
    pub fn is_unknown_dirty(&self) -> bool {
        matches!(self, Self::UnknownDirty)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct VerificationBinding {
    pub workspace_id: WorkspaceId,
    pub workspace_snapshot_id: WorkspaceSnapshotId,
    pub verification_scope_hash: VerificationScopeHash,
    pub check_spec_hash: String,
    pub environment_fingerprint: EnvironmentFingerprint,
    pub sandbox_profile_hash: SandboxProfileHash,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_backend: Option<ExecutionBackendKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_backend_capabilities: Option<ExecutionBackendCapabilities>,
    pub workspace_trust_snapshot_id: WorkspaceTrustSnapshotId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_event_id: Option<EventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_decision_id: Option<EventId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct WorkspaceSnapshotManifestV1 {
    pub workspace_id: WorkspaceId,
    pub scope_hash: VerificationScopeHash,
    pub entries: Vec<WorkspaceSnapshotEntry>,
}

/// Result of building a verification-scope workspace snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct WorkspaceSnapshotBuild {
    pub manifest: WorkspaceSnapshotManifestV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_snapshot_id: Option<WorkspaceSnapshotId>,
    pub workspace_knowledge: WorkspaceKnowledge,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown_dirty_evidence: Option<WorkspaceMutationEvidence>,
}

/// Builds a content-bound workspace snapshot for a verification scope.
///
/// # Errors
///
/// Returns an error when the workspace root cannot be canonicalized or when scope patterns are
/// invalid. Incomplete per-file coverage is represented as `UnknownDirty` in the returned value.
pub fn build_workspace_snapshot(
    workspace_root: impl AsRef<Path>,
    workspace_id: impl Into<WorkspaceId>,
    scope: &VerificationScope,
    workspace_revision: WorkspaceRevision,
) -> Result<WorkspaceSnapshotBuild> {
    build_workspace_snapshot_inner(
        workspace_root.as_ref(),
        workspace_id.into(),
        scope,
        workspace_revision,
        None,
    )
}

/// Builds a snapshot and records incomplete coverage as auditable unknown-dirty evidence.
///
/// # Errors
///
/// Returns an error when the workspace root cannot be canonicalized or when scope patterns are
/// invalid.
pub fn build_workspace_snapshot_for_event(
    workspace_root: impl AsRef<Path>,
    workspace_id: impl Into<WorkspaceId>,
    scope: &VerificationScope,
    workspace_revision: WorkspaceRevision,
    source_event_id: impl Into<EventId>,
    recorded_at_stream_sequence: u64,
) -> Result<WorkspaceSnapshotBuild> {
    if recorded_at_stream_sequence == 0 {
        bail!("workspace snapshot source stream sequence must be non-zero");
    }
    build_workspace_snapshot_inner(
        workspace_root.as_ref(),
        workspace_id.into(),
        scope,
        workspace_revision,
        Some((source_event_id.into(), recorded_at_stream_sequence)),
    )
}

fn build_workspace_snapshot_inner(
    workspace_root: &Path,
    workspace_id: WorkspaceId,
    scope: &VerificationScope,
    workspace_revision: WorkspaceRevision,
    source_event: Option<(EventId, u64)>,
) -> Result<WorkspaceSnapshotBuild> {
    let canonical_root = fs::canonicalize(workspace_root)
        .with_context(|| format!("failed to canonicalize {}", workspace_root.display()))?;
    let include_set = glob_set(&scope.include)?;
    let exclude_patterns = effective_exclude_patterns(scope);
    let exclude_set = glob_set(&exclude_patterns)?;
    let mut entries = Vec::new();
    if let Some(paths) = git_snapshot_paths(&canonical_root, scope) {
        collect_snapshot_entries_for_paths(
            &canonical_root,
            paths,
            &include_set,
            &exclude_set,
            scope.max_file_bytes,
            &mut entries,
        );
    } else {
        collect_snapshot_entries(
            &canonical_root,
            &canonical_root,
            &include_set,
            &exclude_set,
            scope.max_file_bytes,
            &mut entries,
        )?;
    }
    add_missing_literal_includes(&canonical_root, scope, &mut entries)?;
    entries.sort_by(|left, right| left.normalized_path.cmp(&right.normalized_path));
    entries.dedup_by(|left, right| left.normalized_path == right.normalized_path);
    let manifest = WorkspaceSnapshotManifestV1 {
        workspace_id,
        scope_hash: scope.scope_hash.clone(),
        entries,
    };
    let workspace_snapshot_id = manifest.workspace_snapshot_id().ok();
    let unknown_dirty_evidence = workspace_snapshot_id
        .is_none()
        .then_some(source_event)
        .flatten()
        .map(
            |(event_id, recorded_at_stream_sequence)| WorkspaceMutationEvidence {
                event_id,
                source_event_type: "workspace_snapshot_incomplete".to_owned(),
                source_label: None,
                recovery_hint: None,
                scope_hash: scope.scope_hash.clone(),
                recorded_at_stream_sequence,
                from_workspace_snapshot_id: None,
                to_workspace_snapshot_id: None,
                tool_effect: ToolEffect::Unknown,
                unknown_dirty: true,
            },
        );
    let workspace_knowledge = if workspace_snapshot_id.is_some() {
        WorkspaceKnowledge::Clean(workspace_revision)
    } else {
        WorkspaceKnowledge::UnknownDirty
    };
    Ok(WorkspaceSnapshotBuild {
        manifest,
        workspace_snapshot_id,
        workspace_knowledge,
        unknown_dirty_evidence,
    })
}

impl WorkspaceSnapshotManifestV1 {
    pub fn workspace_snapshot_id(&self) -> Result<WorkspaceSnapshotId> {
        if self.entries.iter().any(|entry| !entry.is_complete()) {
            bail!("workspace snapshot manifest contains incomplete entries");
        }
        let mut normalized = self.clone();
        normalized
            .entries
            .sort_by(|left, right| left.normalized_path.cmp(&right.normalized_path));
        let value = serde_json::to_value(&normalized).map_err(|error| {
            anyhow!("failed to convert workspace snapshot manifest to json: {error}")
        })?;
        let bytes = canonical_json_bytes(&value)?;
        let digest = Sha256::digest(bytes);
        Ok(format!("sha256:jcs-v1:{digest:x}"))
    }
}

fn glob_set(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(
            Glob::new(pattern)
                .with_context(|| format!("invalid verification scope pattern {pattern:?}"))?,
        );
    }
    builder
        .build()
        .context("failed to build verification scope matcher")
}

fn effective_exclude_patterns(scope: &VerificationScope) -> Vec<String> {
    let mut patterns = scope.exclude.clone();
    for root in &scope.generated_roots {
        let root = root.to_string_lossy().replace('\\', "/");
        patterns.push(root.clone());
        patterns.push(format!("{root}/**"));
    }
    patterns
}

fn git_snapshot_paths(workspace_root: &Path, scope: &VerificationScope) -> Option<Vec<PathBuf>> {
    if !scope.tracked_files_only || !scope.include.is_empty() {
        return None;
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut paths = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|raw| !raw.is_empty())
        .filter_map(|raw| std::str::from_utf8(raw).ok())
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    Some(paths)
}

fn collect_snapshot_entries_for_paths(
    workspace_root: &Path,
    paths: Vec<PathBuf>,
    include_set: &GlobSet,
    exclude_set: &GlobSet,
    max_file_bytes: u64,
    entries: &mut Vec<WorkspaceSnapshotEntry>,
) {
    for relative in paths {
        if is_excluded(&relative, exclude_set) || !is_included(&relative, include_set) {
            continue;
        }
        let path = workspace_root.join(&relative);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => {
                entries.push(snapshot_entry(
                    relative,
                    FileType::File,
                    SnapshotEntryState::Missing,
                    None,
                    None,
                    None,
                ));
                continue;
            }
        };
        entries.push(snapshot_entry_for_path(
            workspace_root,
            &path,
            relative,
            &metadata,
            max_file_bytes,
        ));
    }
}

fn collect_snapshot_entries(
    workspace_root: &Path,
    dir: &Path,
    include_set: &GlobSet,
    exclude_set: &GlobSet,
    max_file_bytes: u64,
    entries: &mut Vec<WorkspaceSnapshotEntry>,
) -> Result<()> {
    let mut children = fs::read_dir(dir)
        .with_context(|| format!("failed to read directory {}", dir.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("failed to read directory entry in {}", dir.display()))?;
    children.sort_by_key(|entry| entry.path());
    for child in children {
        let path = child.path();
        let relative = normalized_relative_path(workspace_root, &path)?;
        if is_excluded(&relative, exclude_set) {
            continue;
        }
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                entries.push(snapshot_entry(
                    relative,
                    FileType::Other,
                    SnapshotEntryState::PermissionDenied,
                    None,
                    None,
                    None,
                ));
                tracing::debug!(path = %path.display(), "failed to stat verification snapshot entry: {error}");
                continue;
            }
        };
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            collect_snapshot_entries(
                workspace_root,
                &path,
                include_set,
                exclude_set,
                max_file_bytes,
                entries,
            )?;
            continue;
        }
        if !is_included(&relative, include_set) {
            continue;
        }
        entries.push(snapshot_entry_for_path(
            workspace_root,
            &path,
            relative,
            &metadata,
            max_file_bytes,
        ));
    }
    Ok(())
}

fn add_missing_literal_includes(
    workspace_root: &Path,
    scope: &VerificationScope,
    entries: &mut Vec<WorkspaceSnapshotEntry>,
) -> Result<()> {
    for include in &scope.include {
        if has_glob_meta(include) {
            continue;
        }
        let path = workspace_root.join(include);
        if path.exists() {
            continue;
        }
        entries.push(snapshot_entry(
            PathBuf::from(include),
            FileType::File,
            SnapshotEntryState::Missing,
            None,
            None,
            None,
        ));
    }
    Ok(())
}

fn snapshot_entry_for_path(
    workspace_root: &Path,
    path: &Path,
    relative: PathBuf,
    metadata: &fs::Metadata,
    max_file_bytes: u64,
) -> WorkspaceSnapshotEntry {
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return snapshot_symlink_entry(workspace_root, path, relative);
    }
    if file_type.is_file() {
        if metadata.len() > max_file_bytes {
            let mut entry = snapshot_entry(
                relative,
                FileType::File,
                SnapshotEntryState::Unsupported,
                None,
                file_mode(metadata),
                None,
            );
            entry.file_metadata = Some(file_metadata_evidence(metadata));
            return entry;
        }
        return match fs::read(path) {
            Ok(bytes) => {
                let mut entry = snapshot_entry(
                    relative,
                    FileType::File,
                    SnapshotEntryState::Present,
                    Some(stable_bytes_hash(&bytes)),
                    file_mode(metadata),
                    None,
                );
                entry.file_metadata = Some(file_metadata_evidence(metadata));
                entry
            }
            Err(error) => {
                tracing::debug!(path = %path.display(), "failed to read verification snapshot file: {error}");
                let mut entry = snapshot_entry(
                    relative,
                    FileType::File,
                    SnapshotEntryState::PermissionDenied,
                    None,
                    file_mode(metadata),
                    None,
                );
                entry.file_metadata = Some(file_metadata_evidence(metadata));
                entry
            }
        };
    }
    let mut entry = snapshot_entry(
        relative,
        FileType::Other,
        SnapshotEntryState::Unsupported,
        None,
        file_mode(metadata),
        None,
    );
    entry.file_metadata = Some(file_metadata_evidence(metadata));
    entry
}

fn snapshot_symlink_entry(
    workspace_root: &Path,
    path: &Path,
    relative: PathBuf,
) -> WorkspaceSnapshotEntry {
    let symlink_target = fs::read_link(path).ok();
    match fs::canonicalize(path) {
        Ok(target) if target.starts_with(workspace_root) => snapshot_entry(
            relative,
            FileType::Symlink,
            SnapshotEntryState::Present,
            None,
            None,
            symlink_target,
        ),
        Ok(_) => snapshot_entry(
            relative,
            FileType::Symlink,
            SnapshotEntryState::External,
            None,
            None,
            symlink_target,
        ),
        Err(_) => snapshot_entry(
            relative,
            FileType::Symlink,
            SnapshotEntryState::Unsupported,
            None,
            None,
            symlink_target,
        ),
    }
}

fn snapshot_entry(
    normalized_path: PathBuf,
    file_type: FileType,
    state: SnapshotEntryState,
    content_hash: Option<String>,
    mode: Option<u32>,
    symlink_target: Option<PathBuf>,
) -> WorkspaceSnapshotEntry {
    WorkspaceSnapshotEntry {
        normalized_path,
        file_type,
        content_hash,
        mode,
        file_metadata: None,
        symlink_target,
        state,
    }
}

fn normalized_relative_path(workspace_root: &Path, path: &Path) -> Result<PathBuf> {
    let relative = path
        .strip_prefix(workspace_root)
        .with_context(|| format!("failed to relativize {}", path.display()))?;
    Ok(relative.components().collect())
}

fn is_included(relative: &Path, include_set: &GlobSet) -> bool {
    include_set.is_empty() || include_set.is_match(relative)
}

fn is_excluded(relative: &Path, exclude_set: &GlobSet) -> bool {
    exclude_set.is_match(relative)
}

fn has_glob_meta(pattern: &str) -> bool {
    pattern.contains('*') || pattern.contains('?') || pattern.contains('[')
}

fn stable_bytes_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("sha256:{digest:x}")
}

#[cfg(unix)]
fn file_mode(metadata: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;

    Some(metadata.permissions().mode())
}

#[cfg(not(unix))]
fn file_mode(_metadata: &fs::Metadata) -> Option<u32> {
    None
}

fn file_metadata_evidence(metadata: &fs::Metadata) -> FileMetadataEvidence {
    let platform = file_metadata_platform();
    let readonly = metadata.permissions().readonly();
    let unix_mode = file_mode(metadata);
    FileMetadataEvidence {
        platform,
        readonly,
        unix_mode,
    }
}

#[cfg(windows)]
const FILE_METADATA_PLATFORM: FileMetadataPlatform = FileMetadataPlatform::Windows;
#[cfg(unix)]
const FILE_METADATA_PLATFORM: FileMetadataPlatform = FileMetadataPlatform::Unix;
#[cfg(not(any(unix, windows)))]
const FILE_METADATA_PLATFORM: FileMetadataPlatform = FileMetadataPlatform::Other;

fn file_metadata_platform() -> FileMetadataPlatform {
    FILE_METADATA_PLATFORM
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct WorkspaceSnapshotEntry {
    pub normalized_path: PathBuf,
    pub file_type: FileType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_metadata: Option<FileMetadataEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symlink_target: Option<PathBuf>,
    pub state: SnapshotEntryState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct FileMetadataEvidence {
    pub platform: FileMetadataPlatform,
    pub readonly: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unix_mode: Option<u32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileMetadataPlatform {
    Unix,
    Windows,
    Other,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileType {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotEntryState {
    Present,
    Missing,
    PermissionDenied,
    External,
    Unsupported,
}

impl SnapshotEntryState {
    pub fn is_clean(self) -> bool {
        matches!(self, Self::Present | Self::Missing)
    }
}

impl WorkspaceSnapshotEntry {
    pub fn is_complete(&self) -> bool {
        match (self.state, self.file_type) {
            (SnapshotEntryState::Present, FileType::File) => self.content_hash.is_some(),
            (SnapshotEntryState::Present, FileType::Symlink) => self.symlink_target.is_some(),
            (SnapshotEntryState::Present, FileType::Directory | FileType::Other) => true,
            (SnapshotEntryState::Missing, _) => true,
            (
                SnapshotEntryState::PermissionDenied
                | SnapshotEntryState::External
                | SnapshotEntryState::Unsupported,
                _,
            ) => false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvidenceReceipt {
    pub receipt_id: ReceiptId,
    pub source_session_id: SessionId,
    pub source_event_id: EventId,
    pub source_event_type: String,
    pub scope: EvidenceScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer_tool_call: Option<ToolCallId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_revision: Option<WorkspaceRevision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_snapshot_id: Option<WorkspaceSnapshotId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_hash: Option<PolicyHash>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changeset_id: Option<ChangesetId>,
    pub status: ReceiptStatus,
    #[serde(default)]
    pub artifact_refs: Vec<ArtifactId>,
    pub redaction_state: RedactionState,
    pub recorded_at_stream_sequence: u64,
}

impl EvidenceReceipt {
    /// Validates minimum cross-session receipt identity required by parent projections.
    ///
    /// # Errors
    ///
    /// Returns an error when the receipt only has a local sequence or lacks source identifiers.
    pub fn validate_source_identity(&self) -> Result<()> {
        if self.source_session_id.trim().is_empty() {
            bail!("evidence receipt is missing source_session_id");
        }
        if self.source_event_id.trim().is_empty() {
            bail!("evidence receipt is missing source_event_id");
        }
        if self.source_event_type.trim().is_empty() {
            bail!("evidence receipt is missing source_event_type");
        }
        if self.recorded_at_stream_sequence == 0 {
            bail!("evidence receipt stream sequence must be non-zero");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum EvidenceScope {
    Run(String),
    Workspace(WorkspaceId),
    Task(String),
    Step(String),
    Agent(String),
    Changeset(ChangesetId),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptStatus {
    Succeeded,
    Failed,
    Skipped,
    Inconclusive,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RedactionState {
    None,
    Redacted,
    ContainsSensitiveMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct VerificationReceipt {
    pub receipt: EvidenceReceipt,
    pub binding: VerificationBinding,
    pub check_spec_id: CheckSpecId,
    pub check_status: ReceiptStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    pub mutates_verification_scope: bool,
}

/// Durable control entry recording a verification receipt produced by a check.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct VerificationRecordedEntry {
    pub receipt: VerificationReceipt,
}

pub type VerificationCheckRunId = String;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationCheckRunStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Skipped,
    Inconclusive,
    Errored,
}

impl VerificationCheckRunStatus {
    pub fn from_receipt_status(status: ReceiptStatus) -> Self {
        match status {
            ReceiptStatus::Succeeded => Self::Succeeded,
            ReceiptStatus::Failed => Self::Failed,
            ReceiptStatus::Skipped => Self::Skipped,
            ReceiptStatus::Inconclusive => Self::Inconclusive,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct VerificationCheckRunEntry {
    pub run_id: VerificationCheckRunId,
    pub scope: EvidenceScope,
    pub check_spec_id: CheckSpecId,
    pub check_spec_hash: String,
    pub status: VerificationCheckRunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_id: Option<ReceiptId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_event_id: Option<EventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl VerificationCheckRunEntry {
    pub fn new(
        run_id: VerificationCheckRunId,
        scope: EvidenceScope,
        check_spec: &CheckSpec,
        status: VerificationCheckRunStatus,
    ) -> Self {
        Self {
            run_id,
            scope,
            check_spec_id: check_spec.check_spec_id.clone(),
            check_spec_hash: check_spec.check_spec_hash.clone(),
            status,
            receipt_id: None,
            source_event_id: None,
            timeout_ms: None,
            reason: None,
        }
    }

    pub fn with_timeout_ms(mut self, timeout_ms: Option<u64>) -> Self {
        self.timeout_ms = timeout_ms;
        self
    }

    pub fn with_terminal_receipt(mut self, receipt: &VerificationReceipt) -> Self {
        self.status = VerificationCheckRunStatus::from_receipt_status(receipt.check_status);
        self.receipt_id = Some(receipt.receipt.receipt_id.clone());
        self.source_event_id = Some(receipt.receipt.source_event_id.clone());
        self.reason = if let Some(reason) = receipt.failure_reason.clone() {
            Some(reason)
        } else if receipt.mutates_verification_scope {
            Some("check mutated verification scope".to_owned())
        } else {
            None
        };
        self
    }

    pub fn with_error(mut self, reason: impl Into<String>) -> Self {
        self.status = VerificationCheckRunStatus::Errored;
        self.reason = Some(reason.into());
        self
    }
}

pub fn verification_check_run_id(
    scope: &EvidenceScope,
    check_spec: &CheckSpec,
    policy_hash: Option<&str>,
    workspace_snapshot_id: Option<&str>,
    attempt_sequence: u64,
) -> Result<VerificationCheckRunId> {
    let scope =
        serde_json::to_string(scope).context("failed to encode verification check scope")?;
    let seed = format!(
        "{}:{}:{}:{}:{}:{}",
        scope,
        check_spec.check_spec_id,
        check_spec.check_spec_hash,
        policy_hash.unwrap_or("-"),
        workspace_snapshot_id.unwrap_or("-"),
        attempt_sequence
    );
    Ok(stable_event_uuid("sigil-verification-check-run", &seed))
}

/// Request for executing one trusted verification check.
#[derive(Debug, Clone)]
pub struct VerificationCheckRunRequest {
    pub workspace_root: PathBuf,
    pub scope: EvidenceScope,
    pub trusted_check: TrustedCheckSpec,
    pub policy: VerificationPolicy,
    pub policy_hash: Option<PolicyHash>,
    pub workspace_trust: WorkspaceTrust,
    pub workspace_trust_snapshot_id: WorkspaceTrustSnapshotId,
    pub workspace_trust_approval_event_id: Option<EventId>,
    pub workspace_trust_sandbox_decision_id: Option<EventId>,
}

/// Executes a trusted verification check and returns the durable verification projection entry.
///
/// The command result is never treated as proof by itself. The returned receipt is bound to a
/// verification-scope workspace snapshot, and a check that mutates that scope is recorded as
/// non-final evidence so the reducer can require a non-writing rerun.
///
/// # Errors
///
/// Returns an error when the workspace cannot be snapshotted, the durable check/command facts
/// cannot be recorded, or the configured command cannot be spawned.
pub async fn run_verification_check(
    session: &mut Session,
    execution_backend: &dyn ExecutionBackend,
    request: VerificationCheckRunRequest,
) -> Result<VerificationRecordedEntry> {
    let workspace_root = fs::canonicalize(&request.workspace_root).with_context(|| {
        format!(
            "failed to canonicalize verification workspace {}",
            request.workspace_root.display()
        )
    })?;
    let workspace_id = stable_workspace_id(&workspace_root)?;
    let check = &request.trusted_check.check_spec;
    let approval_event_id = request
        .workspace_trust_approval_event_id
        .clone()
        .or_else(|| request.trusted_check.approval_event_id.clone());
    let sandbox_decision_id = request
        .workspace_trust_sandbox_decision_id
        .clone()
        .or_else(|| request.trusted_check.sandbox_decision_id.clone());
    if !request.policy.workspace_trust_requirement.is_satisfied(
        request.workspace_trust,
        approval_event_id.as_ref(),
        sandbox_decision_id.as_ref(),
    ) {
        bail!(
            "verification check {} cannot run until workspace trust requirement is satisfied",
            check.check_spec_id
        );
    }
    let before_snapshot = build_workspace_snapshot(
        &workspace_root,
        workspace_id.clone(),
        &request.policy.verification_scope,
        0,
    )?;

    let started_at = Instant::now();
    let command_output = execute_check_command(
        execution_backend,
        &workspace_root,
        &check.command,
        request.policy.timeout_ms,
    )
    .await?;
    let elapsed_ms = started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;

    let command_event =
        append_command_finished_event(session, check, &request.scope, &command_output, elapsed_ms)?;
    let after_snapshot = build_workspace_snapshot(
        &workspace_root,
        workspace_id.clone(),
        &request.policy.verification_scope,
        0,
    )?;
    let mutates_verification_scope = check.effect.may_mutate_workspace()
        || before_snapshot.workspace_snapshot_id != after_snapshot.workspace_snapshot_id
        || before_snapshot.workspace_knowledge.is_unknown_dirty()
        || after_snapshot.workspace_knowledge.is_unknown_dirty();
    let mutation_event = if mutates_verification_scope {
        append_check_workspace_mutation_detected_event(
            session,
            check,
            &request.scope,
            command_event.as_ref(),
            &workspace_id,
            &before_snapshot,
            &after_snapshot,
        )?
    } else {
        None
    };
    let check_status = check_receipt_status(&command_output, mutates_verification_scope);
    let failure_reason = check_failure_reason(&command_output, request.policy.timeout_ms);
    let check_event = append_check_finished_event(
        session,
        check,
        &request.scope,
        command_event.as_ref(),
        &before_snapshot,
        &after_snapshot,
        check_status,
        mutates_verification_scope,
        mutation_event.as_ref(),
    )?;
    let (source_session_id, source_event_id, recorded_at_stream_sequence) =
        check_event_identity(session, check, &request.scope, check_event.as_ref());
    let current_snapshot_id = after_snapshot
        .workspace_snapshot_id
        .clone()
        .unwrap_or_else(|| {
            stable_event_uuid(
                "sigil-verification-incomplete-snapshot",
                &format!(
                    "{}:{}:{}",
                    source_session_id, source_event_id, recorded_at_stream_sequence
                ),
            )
        });
    let receipt_id = stable_event_uuid(
        "sigil-verification-receipt",
        &format!(
            "{}:{}:{}:{}",
            source_session_id, source_event_id, check.check_spec_id, current_snapshot_id
        ),
    );
    let verification_receipt = VerificationReceipt {
        receipt: EvidenceReceipt {
            receipt_id,
            source_session_id,
            source_event_id,
            source_event_type: DurableEventType::CheckFinished.as_str().to_owned(),
            scope: request.scope,
            producer_tool_call: None,
            workspace_revision: Some(0),
            workspace_snapshot_id: Some(current_snapshot_id.clone()),
            policy_hash: request.policy_hash,
            changeset_id: None,
            status: check_status,
            artifact_refs: Vec::new(),
            redaction_state: RedactionState::None,
            recorded_at_stream_sequence,
        },
        binding: VerificationBinding {
            workspace_id,
            workspace_snapshot_id: current_snapshot_id,
            verification_scope_hash: request.policy.verification_scope.scope_hash,
            check_spec_hash: check.check_spec_hash.clone(),
            environment_fingerprint: environment_fingerprint(check),
            sandbox_profile_hash: sandbox_profile_hash_for_execution(
                request.policy.sandbox_profile,
                command_output.backend,
                command_output.backend_capabilities,
            ),
            execution_backend: Some(command_output.backend),
            execution_backend_capabilities: Some(command_output.backend_capabilities),
            workspace_trust_snapshot_id: request.workspace_trust_snapshot_id,
            approval_event_id,
            sandbox_decision_id,
        },
        check_spec_id: check.check_spec_id.clone(),
        check_status,
        failure_reason,
        mutates_verification_scope,
    };
    Ok(VerificationRecordedEntry {
        receipt: verification_receipt,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CheckCommandOutput {
    backend: ExecutionBackendKind,
    backend_capabilities: ExecutionBackendCapabilities,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

impl CheckCommandOutput {
    fn succeeded(&self) -> bool {
        self.exit_code == Some(0) && !self.timed_out
    }
}

async fn execute_check_command(
    execution_backend: &dyn ExecutionBackend,
    workspace_root: &Path,
    command: &CheckCommand,
    timeout_ms: Option<u64>,
) -> Result<CheckCommandOutput> {
    let cwd = command
        .cwd
        .as_ref()
        .map(|cwd| workspace_root.join(cwd))
        .unwrap_or_else(|| workspace_root.to_path_buf());
    let request = ExecutionRequest {
        program: command.command.clone(),
        args: command.args.clone(),
        cwd: cwd.clone(),
        env: BTreeMap::new(),
        timeout_ms,
        timeout_secs: timeout_ms
            .map(|timeout_ms| timeout_ms.saturating_add(999) / 1000)
            .unwrap_or(0),
    };
    let receipt = execution_backend.execute(request).await.with_context(|| {
        format!(
            "failed to spawn verification check {} in {}",
            format_check_command(command),
            cwd.display()
        )
    })?;
    Ok(CheckCommandOutput {
        backend: receipt.backend,
        backend_capabilities: receipt.capabilities,
        exit_code: receipt.exit_code,
        stdout: truncated_lossy(&receipt.stdout),
        stderr: truncated_lossy(&receipt.stderr),
        timed_out: receipt.timed_out,
    })
}

fn check_receipt_status(
    command_output: &CheckCommandOutput,
    mutates_verification_scope: bool,
) -> ReceiptStatus {
    if command_output.succeeded() {
        if mutates_verification_scope {
            ReceiptStatus::Inconclusive
        } else {
            ReceiptStatus::Succeeded
        }
    } else {
        ReceiptStatus::Failed
    }
}

fn check_failure_reason(
    command_output: &CheckCommandOutput,
    timeout_ms: Option<u64>,
) -> Option<String> {
    if command_output.succeeded() {
        return None;
    }
    if command_output.timed_out {
        return Some(match timeout_ms {
            Some(timeout_ms) => format!("check timed out after {timeout_ms} ms"),
            None => "check timed out".to_owned(),
        });
    }
    Some(match command_output.exit_code {
        Some(code) => format!("check exited with code {code}"),
        None => "check terminated without exit code".to_owned(),
    })
}

fn append_command_finished_event(
    session: &mut Session,
    check: &CheckSpec,
    scope: &EvidenceScope,
    command_output: &CheckCommandOutput,
    elapsed_ms: u64,
) -> Result<Option<StoredEvent>> {
    session.append_durable_event(
        DurableEventType::CommandFinished,
        EventClass::Critical,
        serde_json::json!({
            "scope": scope,
            "check_spec_id": check.check_spec_id,
            "check_spec_hash": check.check_spec_hash,
            "command": check.command.command,
            "args": check.command.args,
            "cwd": check.command.cwd,
            "exit_code": command_output.exit_code,
            "timed_out": command_output.timed_out,
            "elapsed_ms": elapsed_ms,
            "execution_backend": command_output.backend,
            "execution_backend_capabilities": command_output.backend_capabilities,
            "stdout_preview": command_output.stdout,
            "stderr_preview": command_output.stderr,
        }),
    )
}

fn append_check_workspace_mutation_detected_event(
    session: &mut Session,
    check: &CheckSpec,
    scope: &EvidenceScope,
    command_event: Option<&StoredEvent>,
    workspace_id: &str,
    before_snapshot: &WorkspaceSnapshotBuild,
    after_snapshot: &WorkspaceSnapshotBuild,
) -> Result<Option<StoredEvent>> {
    let (reason, unknown_dirty) = if before_snapshot.workspace_knowledge.is_unknown_dirty() {
        ("snapshot_incomplete_before", true)
    } else if after_snapshot.workspace_knowledge.is_unknown_dirty() {
        ("snapshot_incomplete_after", true)
    } else if before_snapshot.workspace_snapshot_id != after_snapshot.workspace_snapshot_id {
        ("snapshot_changed", false)
    } else {
        ("declared_write_effect", true)
    };
    let seed = format!(
        "{scope:?}:{}:{}:{:?}:{:?}",
        check.check_spec_hash,
        command_event
            .map(|event| event.event_id.as_str())
            .unwrap_or("in-memory"),
        before_snapshot.workspace_snapshot_id,
        after_snapshot.workspace_snapshot_id,
    );
    let operation_id = stable_event_uuid("sigil-verification-mutation", &seed);
    session.append_durable_event(
        DurableEventType::WorkspaceMutationDetected,
        EventClass::Critical,
        serde_json::json!({
            "operation_id": operation_id,
            "tool_call_id": null,
            "tool_name": format!("verification_check:{}", check.check_spec_id),
            "tool_effect": check.effect,
            "workspace_id": workspace_id,
            "scope_hash": check.verification_scope_hash,
            "from_workspace_snapshot_id": before_snapshot.workspace_snapshot_id,
            "to_workspace_snapshot_id": after_snapshot.workspace_snapshot_id,
            "base_workspace_revision": 0,
            "workspace_revision": 1,
            "reason": reason,
            "unknown_dirty": unknown_dirty,
        }),
    )
}

fn append_check_finished_event(
    session: &mut Session,
    check: &CheckSpec,
    scope: &EvidenceScope,
    command_event: Option<&StoredEvent>,
    before_snapshot: &WorkspaceSnapshotBuild,
    after_snapshot: &WorkspaceSnapshotBuild,
    status: ReceiptStatus,
    mutates_verification_scope: bool,
    mutation_event: Option<&StoredEvent>,
) -> Result<Option<StoredEvent>> {
    session.append_durable_event(
        DurableEventType::CheckFinished,
        EventClass::Critical,
        serde_json::json!({
            "scope": scope,
            "check_spec_id": check.check_spec_id,
            "check_spec_hash": check.check_spec_hash,
            "command_event_id": command_event.map(|event| event.event_id.as_str()),
            "before_workspace_snapshot_id": before_snapshot.workspace_snapshot_id,
            "after_workspace_snapshot_id": after_snapshot.workspace_snapshot_id,
            "before_workspace_knowledge": before_snapshot.workspace_knowledge,
            "after_workspace_knowledge": after_snapshot.workspace_knowledge,
            "status": status,
            "mutates_verification_scope": mutates_verification_scope,
            "workspace_mutation_detected_event_id": mutation_event.map(|event| event.event_id.as_str()),
        }),
    )
}

fn check_event_identity(
    session: &Session,
    check: &CheckSpec,
    scope: &EvidenceScope,
    check_event: Option<&StoredEvent>,
) -> (SessionId, EventId, u64) {
    if let Some(event) = check_event {
        return (
            event.session_id.clone(),
            event.event_id.clone(),
            event.stream_sequence,
        );
    }
    let sequence = session.entries().len() as u64 + 1;
    let event_id = stable_event_uuid(
        "sigil-check-finished-memory",
        &format!("{scope:?}:{}:{sequence}", check.check_spec_hash),
    );
    ("session:in-memory".to_owned(), event_id, sequence)
}

fn environment_fingerprint(check: &CheckSpec) -> EnvironmentFingerprint {
    stable_hash_parts(
        "env",
        env::consts::OS,
        [env::consts::ARCH, check.command.command.as_str()],
        check
            .command
            .cwd
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default()
            .as_str(),
        &check.command.args.join("\0"),
        "v1",
    )
}

#[cfg(test)]
fn sandbox_profile_hash(requirement: SandboxProfileRequirement) -> SandboxProfileHash {
    stable_hash_parts(
        "sandbox",
        requirement.as_str(),
        std::iter::empty::<&str>(),
        "",
        "",
        "v1",
    )
}

fn sandbox_profile_hash_for_execution(
    requirement: SandboxProfileRequirement,
    backend: ExecutionBackendKind,
    capabilities: ExecutionBackendCapabilities,
) -> SandboxProfileHash {
    let filesystem_isolation = capability_bit("filesystem", capabilities.filesystem_isolation);
    let network_isolation = capability_bit("network", capabilities.network_isolation);
    let process_isolation = capability_bit("process", capabilities.process_isolation);
    let resource_limits = capability_bit("resource_limits", capabilities.resource_limits);
    let persistent_pty = capability_bit("persistent_pty", capabilities.persistent_pty);
    let workspace_snapshot = capability_bit("workspace_snapshot", capabilities.workspace_snapshot);
    stable_hash_parts(
        "sandbox",
        requirement.as_str(),
        [
            backend.as_str(),
            filesystem_isolation.as_str(),
            network_isolation.as_str(),
            process_isolation.as_str(),
            resource_limits.as_str(),
            persistent_pty.as_str(),
            workspace_snapshot.as_str(),
        ],
        "",
        "",
        "v2",
    )
}

fn capability_bit(name: &str, value: bool) -> String {
    format!("{name}={value}")
}

impl SandboxProfileRequirement {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ApprovalOrSandbox => "approval_or_sandbox",
            Self::Sandboxed => "sandboxed",
        }
    }
}

fn format_check_command(command: &CheckCommand) -> String {
    std::iter::once(command.command.as_str())
        .chain(command.args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

fn truncated_lossy(bytes: &[u8]) -> String {
    const MAX_PREVIEW_BYTES: usize = 4096;
    let mut value = String::from_utf8_lossy(bytes).into_owned();
    if value.len() > MAX_PREVIEW_BYTES {
        value.truncate(MAX_PREVIEW_BYTES);
        value.push_str("\n[truncated]");
    }
    value
}

impl VerificationReceipt {
    pub fn is_applicable_to(
        &self,
        check: &CheckSpec,
        current_snapshot_id: &WorkspaceSnapshotId,
        scope: &VerificationScope,
        trust_requirement: WorkspaceTrustRequirement,
        workspace_trust: WorkspaceTrust,
        sandbox_requirement: SandboxProfileRequirement,
    ) -> bool {
        self.check_spec_id == check.check_spec_id
            && self.binding.check_spec_hash == check.check_spec_hash
            && self.binding.workspace_snapshot_id == *current_snapshot_id
            && self.binding.verification_scope_hash == scope.scope_hash
            && self.receipt.workspace_snapshot_id.as_ref() == Some(current_snapshot_id)
            && !self.mutates_verification_scope
            && receipt_satisfies_execution_trust(self, trust_requirement, workspace_trust)
            && receipt_satisfies_sandbox_profile(self, sandbox_requirement)
    }
}

fn receipt_satisfies_execution_trust(
    receipt: &VerificationReceipt,
    trust_requirement: WorkspaceTrustRequirement,
    workspace_trust: WorkspaceTrust,
) -> bool {
    match trust_requirement {
        WorkspaceTrustRequirement::None => true,
        WorkspaceTrustRequirement::ApprovalOrSandbox => {
            workspace_trust == WorkspaceTrust::Trusted
                || receipt.binding.approval_event_id.is_some()
                || receipt.binding.sandbox_decision_id.is_some()
        }
        WorkspaceTrustRequirement::Trusted => workspace_trust == WorkspaceTrust::Trusted,
    }
}

fn receipt_matches_current_context(
    receipt: &VerificationReceipt,
    check: &CheckSpec,
    current_snapshot_id: &WorkspaceSnapshotId,
    scope: &VerificationScope,
    trust_requirement: WorkspaceTrustRequirement,
    workspace_trust: WorkspaceTrust,
    sandbox_requirement: SandboxProfileRequirement,
) -> bool {
    receipt.check_spec_id == check.check_spec_id
        && receipt.binding.check_spec_hash == check.check_spec_hash
        && receipt.binding.workspace_snapshot_id == *current_snapshot_id
        && receipt.binding.verification_scope_hash == scope.scope_hash
        && receipt.receipt.workspace_snapshot_id.as_ref() == Some(current_snapshot_id)
        && receipt_satisfies_execution_trust(receipt, trust_requirement, workspace_trust)
        && receipt_satisfies_sandbox_profile(receipt, sandbox_requirement)
}

fn receipt_satisfies_sandbox_profile(
    receipt: &VerificationReceipt,
    requirement: SandboxProfileRequirement,
) -> bool {
    match requirement {
        SandboxProfileRequirement::None => true,
        SandboxProfileRequirement::ApprovalOrSandbox => {
            receipt.binding.approval_event_id.is_some()
                || receipt.binding.sandbox_decision_id.is_some()
                || receipt_has_matching_sandbox_backend(receipt, requirement)
        }
        SandboxProfileRequirement::Sandboxed => {
            receipt_has_matching_sandbox_backend(receipt, requirement)
        }
    }
}

fn receipt_has_matching_sandbox_backend(
    receipt: &VerificationReceipt,
    requirement: SandboxProfileRequirement,
) -> bool {
    let Some(backend) = receipt.binding.execution_backend else {
        return false;
    };
    let Some(capabilities) = receipt.binding.execution_backend_capabilities else {
        return false;
    };
    capabilities.supports_required_sandbox()
        && receipt.binding.sandbox_profile_hash
            == sandbox_profile_hash_for_execution(requirement, backend, capabilities)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct WorkspaceMutationEvidence {
    pub event_id: EventId,
    pub source_event_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_hint: Option<String>,
    pub scope_hash: VerificationScopeHash,
    pub recorded_at_stream_sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_workspace_snapshot_id: Option<WorkspaceSnapshotId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_workspace_snapshot_id: Option<WorkspaceSnapshotId>,
    pub tool_effect: ToolEffect,
    pub unknown_dirty: bool,
}

impl WorkspaceMutationEvidence {
    pub fn from_detected_event(
        event_id: EventId,
        recorded_at_stream_sequence: u64,
        payload: WorkspaceMutationDetected,
    ) -> Self {
        let (source_label, recovery_hint) = unknown_mutation_source_context(&payload.tool_name);
        Self {
            event_id,
            source_event_type: DurableEventType::WorkspaceMutationDetected
                .as_str()
                .to_owned(),
            source_label,
            recovery_hint,
            scope_hash: payload.scope_hash,
            recorded_at_stream_sequence,
            from_workspace_snapshot_id: payload.from_workspace_snapshot_id,
            to_workspace_snapshot_id: payload.to_workspace_snapshot_id,
            tool_effect: payload.tool_effect,
            unknown_dirty: payload.unknown_dirty,
        }
    }

    pub fn invalidates_scope(&self, scope: &VerificationScope) -> bool {
        self.unknown_dirty || self.scope_hash == scope.scope_hash
    }

    fn source_readiness_reason(&self) -> Option<ReadinessReason> {
        if !self.unknown_dirty {
            return None;
        }
        Some(ReadinessReason::WorkspaceMutationSource {
            event_id: self.event_id.clone(),
            source_label: self.source_label.clone()?,
            recovery_hint: self.recovery_hint.clone(),
        })
    }
}

fn unknown_mutation_source_context(tool_name: &str) -> (Option<String>, Option<String>) {
    if let Some(server_name) = tool_name.strip_prefix("mcp_server:") {
        return (
            Some(format!("MCP server {server_name}")),
            Some("refresh MCP or run check".to_owned()),
        );
    }
    (None, None)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "reason", content = "event_id")]
pub enum VerificationStaleReason {
    WorkspaceChanged(EventId),
    CheckSpecChanged(EventId),
    PolicyChanged(EventId),
    EnvironmentChanged(EventId),
    SandboxChanged(EventId),
    TrustChanged(EventId),
    UnknownDirty(EventId),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct VerificationStaleCause {
    pub reason: VerificationStaleReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_workspace_snapshot_id: Option<WorkspaceSnapshotId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_workspace_snapshot_id: Option<WorkspaceSnapshotId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct VerificationSkipDecision {
    pub event_id: EventId,
    pub reason: String,
}

/// Durable control entry recording a workspace trust decision relevant to verification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct WorkspaceTrustDecisionEntry {
    pub workspace_id: WorkspaceId,
    pub workspace_trust_snapshot_id: WorkspaceTrustSnapshotId,
    pub trust: WorkspaceTrust,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by_event_id: Option<EventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ReadinessInput {
    pub run_status: RunStatus,
    pub projection_mode: ReadinessProjectionMode,
    pub policy: VerificationPolicy,
    pub workspace_trust: WorkspaceTrust,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_trust_approval_event_id: Option<EventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_trust_sandbox_decision_id: Option<EventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_workspace_snapshot_id: Option<WorkspaceSnapshotId>,
    pub workspace_knowledge: WorkspaceKnowledge,
    #[serde(default)]
    pub verification_receipts: Vec<VerificationReceipt>,
    #[serde(default)]
    pub mutations: Vec<WorkspaceMutationEvidence>,
    #[serde(default)]
    pub stale_causes: Vec<VerificationStaleCause>,
    #[serde(default)]
    pub pending_checks: Vec<CheckSpecId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_decision: Option<VerificationSkipDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_assistant_event_id: Option<EventId>,
    #[serde(default)]
    pub recovered_tool_error_event_ids: Vec<EventId>,
}

impl ReadinessInput {
    pub fn new_run(run_status: RunStatus, policy: VerificationPolicy) -> Self {
        Self {
            run_status,
            projection_mode: ReadinessProjectionMode::NewRun,
            policy,
            workspace_trust: WorkspaceTrust::Unknown,
            workspace_trust_approval_event_id: None,
            workspace_trust_sandbox_decision_id: None,
            current_workspace_snapshot_id: None,
            workspace_knowledge: WorkspaceKnowledge::Clean(0),
            verification_receipts: Vec::new(),
            mutations: Vec::new(),
            stale_causes: Vec::new(),
            pending_checks: Vec::new(),
            skip_decision: None,
            final_assistant_event_id: None,
            recovered_tool_error_event_ids: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessProjectionMode {
    NewRun,
    LegacyProjection,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ReadinessEvaluation {
    pub run_status: RunStatus,
    pub verification_verdict: VerificationVerdict,
    pub visible_state: VisibleCompletionState,
    #[serde(default)]
    pub reasons: Vec<ReadinessReason>,
    #[serde(default)]
    pub required_actions: Vec<RequiredAction>,
}

/// Durable control entry recording a system-computed readiness verdict.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ReadinessEvaluatedEntry {
    pub scope: EvidenceScope,
    pub evaluation: ReadinessEvaluation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_hash: Option<PolicyHash>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_snapshot_id: Option<WorkspaceSnapshotId>,
}

/// Materialized verification view reconstructed from append-only control entries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerificationStateProjection {
    pub check_specs: BTreeMap<(EvidenceScope, CheckSpecId), CheckSpecRecordedEntry>,
    pub policies: BTreeMap<EvidenceScope, VerificationPolicyChangedEntry>,
    pub check_runs: BTreeMap<VerificationCheckRunId, VerificationCheckRunEntry>,
    pub receipts: BTreeMap<ReceiptId, VerificationRecordedEntry>,
    pub readiness: BTreeMap<EvidenceScope, ReadinessEvaluatedEntry>,
    pub child_receipt_links: Vec<ChildVerificationReceiptLinked>,
    pub workspace_trust: BTreeMap<WorkspaceId, WorkspaceTrustDecisionEntry>,
}

impl VerificationStateProjection {
    /// Replays session entries into a verification projection.
    pub fn from_entries(entries: &[SessionLogEntry]) -> Self {
        let mut projection = Self::default();
        for entry in entries {
            let SessionLogEntry::Control(control) = entry else {
                continue;
            };
            projection.apply_control_entry(control);
        }
        projection
    }

    pub fn latest_policy(&self, scope: &EvidenceScope) -> Option<&VerificationPolicyChangedEntry> {
        self.policies.get(scope)
    }

    pub fn latest_readiness(&self, scope: &EvidenceScope) -> Option<&ReadinessEvaluatedEntry> {
        self.readiness.get(scope)
    }

    pub fn receipt(&self, receipt_id: &str) -> Option<&VerificationRecordedEntry> {
        self.receipts.get(receipt_id)
    }

    pub fn check_run(&self, run_id: &str) -> Option<&VerificationCheckRunEntry> {
        self.check_runs.get(run_id)
    }

    pub fn check_spec(
        &self,
        scope: &EvidenceScope,
        check_spec_id: &str,
    ) -> Option<&CheckSpecRecordedEntry> {
        self.check_specs
            .get(&(scope.clone(), check_spec_id.to_owned()))
    }

    pub fn check_specs_for_scopes(
        &self,
        scopes_by_precedence: &[EvidenceScope],
    ) -> Vec<&CheckSpecRecordedEntry> {
        let mut selected = BTreeMap::<CheckSpecId, &CheckSpecRecordedEntry>::new();
        for scope in scopes_by_precedence.iter().rev() {
            for ((entry_scope, check_spec_id), entry) in &self.check_specs {
                if entry_scope == scope {
                    selected.insert(check_spec_id.clone(), entry);
                }
            }
        }
        selected.into_values().collect()
    }

    pub fn apply_control_entry(&mut self, control: &ControlEntry) {
        match control {
            ControlEntry::CheckSpecRecorded(entry) => {
                self.check_specs.insert(
                    (
                        entry.scope.clone(),
                        entry.trusted_check.check_spec.check_spec_id.clone(),
                    ),
                    entry.clone(),
                );
            }
            ControlEntry::VerificationPolicyChanged(entry) => {
                self.policies.insert(entry.scope.clone(), entry.clone());
            }
            ControlEntry::VerificationCheckRun(entry) => {
                self.check_runs.insert(entry.run_id.clone(), entry.clone());
            }
            ControlEntry::VerificationRecorded(entry) => {
                self.receipts
                    .insert(entry.receipt.receipt.receipt_id.clone(), entry.clone());
            }
            ControlEntry::ReadinessEvaluated(entry) => {
                self.readiness.insert(entry.scope.clone(), entry.clone());
            }
            ControlEntry::ChildVerificationReceiptLinked(entry) => {
                self.child_receipt_links.push(entry.clone());
            }
            ControlEntry::WorkspaceTrustDecision(entry) => {
                self.workspace_trust
                    .insert(entry.workspace_id.clone(), entry.clone());
            }
            _ => {}
        }
    }

    pub fn apply_control(&mut self, control: &ControlEntry) {
        self.apply_control_entry(control);
    }
}

/// JSON-friendly persisted form of `VerificationStateProjection`.
///
/// The runtime projection uses map keys that are not ideal JSON object keys. The persisted snapshot
/// keeps the same materialized facts as ordered entry vectors so a projection store can be rebuilt
/// from JSONL and reloaded without reparsing the full session stream.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct VerificationStateProjectionSnapshot {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub check_specs: Vec<CheckSpecRecordedEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policies: Vec<VerificationPolicyChangedEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub check_runs: Vec<VerificationCheckRunEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub receipts: Vec<VerificationRecordedEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub readiness: Vec<ReadinessEvaluatedEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_receipt_links: Vec<ChildVerificationReceiptLinked>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_trust: Vec<WorkspaceTrustDecisionEntry>,
}

impl From<&VerificationStateProjection> for VerificationStateProjectionSnapshot {
    fn from(projection: &VerificationStateProjection) -> Self {
        Self {
            check_specs: projection.check_specs.values().cloned().collect(),
            policies: projection.policies.values().cloned().collect(),
            check_runs: projection.check_runs.values().cloned().collect(),
            receipts: projection.receipts.values().cloned().collect(),
            readiness: projection.readiness.values().cloned().collect(),
            child_receipt_links: projection.child_receipt_links.clone(),
            workspace_trust: projection.workspace_trust.values().cloned().collect(),
        }
    }
}

impl From<VerificationStateProjectionSnapshot> for VerificationStateProjection {
    fn from(snapshot: VerificationStateProjectionSnapshot) -> Self {
        let mut projection = Self::default();
        for entry in snapshot.check_specs {
            projection.apply_control_entry(&ControlEntry::CheckSpecRecorded(entry));
        }
        for entry in snapshot.policies {
            projection.apply_control_entry(&ControlEntry::VerificationPolicyChanged(entry));
        }
        for entry in snapshot.check_runs {
            projection.apply_control_entry(&ControlEntry::VerificationCheckRun(entry));
        }
        for entry in snapshot.receipts {
            projection.apply_control_entry(&ControlEntry::VerificationRecorded(entry));
        }
        for entry in snapshot.readiness {
            projection.apply_control_entry(&ControlEntry::ReadinessEvaluated(entry));
        }
        for entry in snapshot.child_receipt_links {
            projection.apply_control_entry(&ControlEntry::ChildVerificationReceiptLinked(entry));
        }
        for entry in snapshot.workspace_trust {
            projection.apply_control_entry(&ControlEntry::WorkspaceTrustDecision(entry));
        }
        projection
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "reason", content = "details")]
pub enum ReadinessReason {
    LegacyEvidenceUnavailable,
    NoVerificationRequired,
    FinalAssistantTextIgnored {
        event_id: EventId,
    },
    RecoveredToolError {
        event_id: EventId,
    },
    WorkspaceTrustUnsatisfied,
    PendingCheckReducedForTerminalRun {
        check_spec_id: CheckSpecId,
    },
    MissingRequiredCheck {
        check_spec_id: CheckSpecId,
    },
    VerificationPassed {
        receipt_id: ReceiptId,
    },
    VerificationFailed {
        receipt_id: ReceiptId,
    },
    VerificationSkipped {
        event_id: EventId,
    },
    VerificationStale(VerificationStaleCause),
    WorkspaceMutationSource {
        event_id: EventId,
        source_label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recovery_hint: Option<String>,
    },
    WorkspaceUnknownDirty {
        event_id: Option<EventId>,
    },
    CheckMutatedVerificationScope {
        check_spec_id: CheckSpecId,
    },
    ReceiptScopeMismatch {
        receipt_id: ReceiptId,
    },
    ReceiptSnapshotMismatch {
        receipt_id: ReceiptId,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "action", content = "details")]
pub enum RequiredAction {
    RunCheck { check_spec_id: CheckSpecId },
    ApproveCheckExecution { check_spec_id: CheckSpecId },
    TrustWorkspace,
    ResolveUnknownDirty,
    ReRunNonWritingCheck { check_spec_id: CheckSpecId },
    ReviewVerificationFailure { receipt_id: ReceiptId },
    ProvideVerificationConfig,
}

/// Computes a verification verdict from typed evidence.
pub fn evaluate_readiness(input: &ReadinessInput) -> ReadinessEvaluation {
    let mut reasons = Vec::new();
    let mut required_actions = Vec::new();

    if let Some(event_id) = &input.final_assistant_event_id {
        reasons.push(ReadinessReason::FinalAssistantTextIgnored {
            event_id: event_id.clone(),
        });
    }
    for event_id in &input.recovered_tool_error_event_ids {
        reasons.push(ReadinessReason::RecoveredToolError {
            event_id: event_id.clone(),
        });
    }

    if input.projection_mode == ReadinessProjectionMode::LegacyProjection {
        reasons.push(ReadinessReason::LegacyEvidenceUnavailable);
        return evaluation(
            input.run_status,
            VerificationVerdict::NotEvaluated,
            reasons,
            required_actions,
        );
    }

    if !input.policy.workspace_trust_requirement.is_satisfied(
        input.workspace_trust,
        input.workspace_trust_approval_event_id.as_ref(),
        input.workspace_trust_sandbox_decision_id.as_ref(),
    ) {
        reasons.push(ReadinessReason::WorkspaceTrustUnsatisfied);
        required_actions.push(RequiredAction::TrustWorkspace);
        return finalize_new_run(
            input.run_status,
            VerificationVerdict::Missing,
            reasons,
            required_actions,
        );
    }

    if !input.pending_checks.is_empty() {
        if input.run_status.is_terminal() {
            for check_spec_id in &input.pending_checks {
                reasons.push(ReadinessReason::PendingCheckReducedForTerminalRun {
                    check_spec_id: check_spec_id.clone(),
                });
                required_actions.push(RequiredAction::RunCheck {
                    check_spec_id: check_spec_id.clone(),
                });
            }
            return finalize_new_run(
                input.run_status,
                VerificationVerdict::Inconclusive,
                reasons,
                required_actions,
            );
        }
        return evaluation(
            input.run_status,
            VerificationVerdict::Pending,
            reasons,
            required_actions,
        );
    }

    if input.policy.required_checks.is_empty() {
        if has_relevant_mutation(input) {
            return missing_for_mutation(input, reasons, required_actions);
        }
        reasons.push(ReadinessReason::NoVerificationRequired);
        return evaluation(
            input.run_status,
            VerificationVerdict::NotApplicable,
            reasons,
            required_actions,
        );
    }

    if let Some(skip) = &input.skip_decision
        && input.policy.allow_unverified_completion
    {
        reasons.push(ReadinessReason::VerificationSkipped {
            event_id: skip.event_id.clone(),
        });
        return evaluation(
            input.run_status,
            VerificationVerdict::Skipped,
            reasons,
            required_actions,
        );
    }

    let prior_passed = input
        .verification_receipts
        .iter()
        .any(|receipt| receipt.check_status == ReceiptStatus::Succeeded);
    if input.workspace_knowledge.is_unknown_dirty() {
        let unknown_dirty_mutation = input
            .mutations
            .iter()
            .find(|mutation| mutation.unknown_dirty);
        if let Some(reason) =
            unknown_dirty_mutation.and_then(WorkspaceMutationEvidence::source_readiness_reason)
        {
            reasons.push(reason);
        }
        let event_id = unknown_dirty_mutation.map(|mutation| mutation.event_id.clone());
        reasons.push(ReadinessReason::WorkspaceUnknownDirty {
            event_id: event_id.clone(),
        });
        required_actions.push(RequiredAction::ResolveUnknownDirty);
        let verdict = if event_id.is_some() {
            if prior_passed {
                VerificationVerdict::Stale
            } else {
                VerificationVerdict::Inconclusive
            }
        } else {
            VerificationVerdict::Inconclusive
        };
        if verdict == VerificationVerdict::Stale {
            reasons.push(ReadinessReason::VerificationStale(VerificationStaleCause {
                reason: VerificationStaleReason::UnknownDirty(
                    input
                        .mutations
                        .iter()
                        .find(|mutation| mutation.unknown_dirty)
                        .map(|mutation| mutation.event_id.clone())
                        .unwrap_or_else(|| "unknown_dirty".to_owned()),
                ),
                from_workspace_snapshot_id: None,
                to_workspace_snapshot_id: None,
            }));
        }
        return finalize_new_run(input.run_status, verdict, reasons, required_actions);
    }

    if let Some(stale_cause) = latest_stale_cause(input) {
        reasons.push(ReadinessReason::VerificationStale(stale_cause));
        return finalize_new_run(
            input.run_status,
            VerificationVerdict::Stale,
            reasons,
            required_actions,
        );
    }

    let Some(current_snapshot_id) = &input.current_workspace_snapshot_id else {
        required_actions.push(RequiredAction::RunCheck {
            check_spec_id: input.policy.required_checks[0].check_spec_id.clone(),
        });
        reasons.push(ReadinessReason::MissingRequiredCheck {
            check_spec_id: input.policy.required_checks[0].check_spec_id.clone(),
        });
        return finalize_new_run(
            input.run_status,
            VerificationVerdict::Missing,
            reasons,
            required_actions,
        );
    };

    let mut any_passed = false;
    let mut first_failed: Option<ReceiptId> = None;
    for check in &input.policy.required_checks {
        let receipts = input
            .verification_receipts
            .iter()
            .filter(|receipt| receipt.check_spec_id == check.check_spec_id)
            .collect::<Vec<_>>();
        let current_receipt = receipts
            .iter()
            .copied()
            .filter(|receipt| {
                receipt_matches_current_context(
                    receipt,
                    check,
                    current_snapshot_id,
                    &input.policy.verification_scope,
                    input.policy.workspace_trust_requirement,
                    input.workspace_trust,
                    input.policy.sandbox_profile,
                )
            })
            .max_by_key(|receipt| receipt.receipt.recorded_at_stream_sequence);

        match current_receipt.map(|receipt| (receipt.check_status, receipt)) {
            Some((ReceiptStatus::Succeeded, receipt)) if !receipt.mutates_verification_scope => {
                any_passed = true;
                reasons.push(ReadinessReason::VerificationPassed {
                    receipt_id: receipt.receipt.receipt_id.clone(),
                });
            }
            Some((ReceiptStatus::Succeeded, _receipt)) => {
                reasons.push(ReadinessReason::CheckMutatedVerificationScope {
                    check_spec_id: check.check_spec_id.clone(),
                });
                required_actions.push(RequiredAction::ReRunNonWritingCheck {
                    check_spec_id: check.check_spec_id.clone(),
                });
                if input.policy.completion_criteria == CompletionCriteria::AllRequiredChecks {
                    return finalize_new_run(
                        input.run_status,
                        VerificationVerdict::Missing,
                        reasons,
                        required_actions,
                    );
                }
            }
            Some((ReceiptStatus::Failed, receipt)) => {
                reasons.push(ReadinessReason::VerificationFailed {
                    receipt_id: receipt.receipt.receipt_id.clone(),
                });
                first_failed.get_or_insert_with(|| receipt.receipt.receipt_id.clone());
                if input.policy.completion_criteria == CompletionCriteria::AllRequiredChecks {
                    required_actions.push(RequiredAction::ReviewVerificationFailure {
                        receipt_id: receipt.receipt.receipt_id.clone(),
                    });
                    return finalize_new_run(
                        input.run_status,
                        VerificationVerdict::Failed,
                        reasons,
                        required_actions,
                    );
                }
            }
            Some((ReceiptStatus::Skipped | ReceiptStatus::Inconclusive, receipt)) => {
                reasons.push(ReadinessReason::MissingRequiredCheck {
                    check_spec_id: check.check_spec_id.clone(),
                });
                required_actions.push(RequiredAction::RunCheck {
                    check_spec_id: check.check_spec_id.clone(),
                });
                if receipt.check_status == ReceiptStatus::Inconclusive
                    && input.policy.completion_criteria == CompletionCriteria::AllRequiredChecks
                {
                    return finalize_new_run(
                        input.run_status,
                        VerificationVerdict::Inconclusive,
                        reasons,
                        required_actions,
                    );
                }
                if input.policy.completion_criteria == CompletionCriteria::AllRequiredChecks {
                    return finalize_new_run(
                        input.run_status,
                        VerificationVerdict::Missing,
                        reasons,
                        required_actions,
                    );
                }
            }
            None => {
                if let Some(receipt) = receipts.first() {
                    if receipt.binding.verification_scope_hash
                        != input.policy.verification_scope.scope_hash
                    {
                        reasons.push(ReadinessReason::ReceiptScopeMismatch {
                            receipt_id: receipt.receipt.receipt_id.clone(),
                        });
                    } else if receipt.binding.workspace_snapshot_id != *current_snapshot_id {
                        reasons.push(ReadinessReason::ReceiptSnapshotMismatch {
                            receipt_id: receipt.receipt.receipt_id.clone(),
                        });
                    }
                }
                reasons.push(ReadinessReason::MissingRequiredCheck {
                    check_spec_id: check.check_spec_id.clone(),
                });
                required_actions.push(RequiredAction::RunCheck {
                    check_spec_id: check.check_spec_id.clone(),
                });
                if input.policy.completion_criteria == CompletionCriteria::AllRequiredChecks {
                    return finalize_new_run(
                        input.run_status,
                        VerificationVerdict::Missing,
                        reasons,
                        required_actions,
                    );
                }
            }
        }
    }

    match input.policy.completion_criteria {
        CompletionCriteria::NoChecksRequired => {
            reasons.push(ReadinessReason::NoVerificationRequired);
            evaluation(
                input.run_status,
                VerificationVerdict::NotApplicable,
                reasons,
                required_actions,
            )
        }
        CompletionCriteria::AnyRequiredCheck if any_passed => evaluation(
            input.run_status,
            VerificationVerdict::Passed,
            reasons,
            required_actions,
        ),
        CompletionCriteria::AllRequiredChecks if any_passed => evaluation(
            input.run_status,
            VerificationVerdict::Passed,
            reasons,
            required_actions,
        ),
        CompletionCriteria::AnyRequiredCheck | CompletionCriteria::AllRequiredChecks => {
            if let Some(receipt_id) = first_failed {
                required_actions.push(RequiredAction::ReviewVerificationFailure {
                    receipt_id: receipt_id.clone(),
                });
                return finalize_new_run(
                    input.run_status,
                    VerificationVerdict::Failed,
                    reasons,
                    required_actions,
                );
            }
            if required_actions.is_empty() {
                required_actions.push(RequiredAction::ProvideVerificationConfig);
            }
            finalize_new_run(
                input.run_status,
                VerificationVerdict::Missing,
                reasons,
                required_actions,
            )
        }
    }
}

fn has_relevant_mutation(input: &ReadinessInput) -> bool {
    matches!(
        input.workspace_knowledge,
        WorkspaceKnowledge::Dirty(_) | WorkspaceKnowledge::UnknownDirty
    ) || input
        .mutations
        .iter()
        .any(|mutation| mutation.invalidates_scope(&input.policy.verification_scope))
}

fn missing_for_mutation(
    input: &ReadinessInput,
    mut reasons: Vec<ReadinessReason>,
    mut required_actions: Vec<RequiredAction>,
) -> ReadinessEvaluation {
    if input.workspace_knowledge.is_unknown_dirty() {
        let unknown_dirty_mutation = input
            .mutations
            .iter()
            .find(|mutation| mutation.unknown_dirty);
        if let Some(reason) =
            unknown_dirty_mutation.and_then(WorkspaceMutationEvidence::source_readiness_reason)
        {
            reasons.push(reason);
        }
        reasons.push(ReadinessReason::WorkspaceUnknownDirty {
            event_id: unknown_dirty_mutation.map(|mutation| mutation.event_id.clone()),
        });
        required_actions.push(RequiredAction::ResolveUnknownDirty);
        return finalize_new_run(
            input.run_status,
            VerificationVerdict::Inconclusive,
            reasons,
            required_actions,
        );
    }
    required_actions.push(RequiredAction::ProvideVerificationConfig);
    finalize_new_run(
        input.run_status,
        VerificationVerdict::Missing,
        reasons,
        required_actions,
    )
}

fn latest_stale_cause(input: &ReadinessInput) -> Option<VerificationStaleCause> {
    if let Some(cause) = input.stale_causes.last() {
        return Some(cause.clone());
    }
    let latest_pass_sequence = input
        .verification_receipts
        .iter()
        .filter(|receipt| receipt.check_status == ReceiptStatus::Succeeded)
        .map(|receipt| receipt.receipt.recorded_at_stream_sequence)
        .max()?;
    input
        .mutations
        .iter()
        .filter(|mutation| {
            mutation.recorded_at_stream_sequence > latest_pass_sequence
                && mutation.invalidates_scope(&input.policy.verification_scope)
        })
        .max_by_key(|mutation| mutation.recorded_at_stream_sequence)
        .map(|mutation| VerificationStaleCause {
            reason: if mutation.unknown_dirty {
                VerificationStaleReason::UnknownDirty(mutation.event_id.clone())
            } else {
                VerificationStaleReason::WorkspaceChanged(mutation.event_id.clone())
            },
            from_workspace_snapshot_id: mutation.from_workspace_snapshot_id.clone(),
            to_workspace_snapshot_id: mutation.to_workspace_snapshot_id.clone(),
        })
}

fn finalize_new_run(
    run_status: RunStatus,
    verdict: VerificationVerdict,
    mut reasons: Vec<ReadinessReason>,
    required_actions: Vec<RequiredAction>,
) -> ReadinessEvaluation {
    let verdict = if run_status.is_terminal() && verdict == VerificationVerdict::Pending {
        reasons.push(ReadinessReason::PendingCheckReducedForTerminalRun {
            check_spec_id: "unknown".to_owned(),
        });
        VerificationVerdict::Inconclusive
    } else if run_status.is_terminal() && verdict == VerificationVerdict::NotEvaluated {
        VerificationVerdict::Missing
    } else {
        verdict
    };
    evaluation(run_status, verdict, reasons, required_actions)
}

fn evaluation(
    run_status: RunStatus,
    verification_verdict: VerificationVerdict,
    reasons: Vec<ReadinessReason>,
    required_actions: Vec<RequiredAction>,
) -> ReadinessEvaluation {
    ReadinessEvaluation {
        run_status,
        verification_verdict,
        visible_state: VisibleCompletionState::derive(run_status, verification_verdict),
        reasons,
        required_actions,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ChildVerificationReceiptLinked {
    pub parent_session_id: SessionId,
    pub child_session_id: SessionId,
    pub child_receipt_id: ReceiptId,
    pub child_event_id: EventId,
    pub child_workspace_id: WorkspaceId,
    pub child_workspace_snapshot_id: WorkspaceSnapshotId,
    pub policy_hash: PolicyHash,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changeset_id: Option<ChangesetId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_event_id: Option<EventId>,
}

impl ChildVerificationReceiptLinked {
    /// Validates that parent projections can trace child evidence across session boundaries.
    ///
    /// # Errors
    ///
    /// Returns an error when mandatory child source identifiers are missing.
    pub fn validate(&self) -> Result<()> {
        if self.parent_session_id.trim().is_empty()
            || self.child_session_id.trim().is_empty()
            || self.child_receipt_id.trim().is_empty()
            || self.child_event_id.trim().is_empty()
            || self.child_workspace_id.trim().is_empty()
            || self.child_workspace_snapshot_id.trim().is_empty()
            || self.policy_hash.trim().is_empty()
        {
            bail!("child verification receipt link is missing required identity");
        }
        Ok(())
    }
}

fn stable_hash_parts<'a>(
    check_spec_id: &'a str,
    command: &'a str,
    args: impl IntoIterator<Item = &'a str>,
    cwd: &'a str,
    scope_hash: &'a str,
    effect: &'a str,
) -> String {
    let mut digest = Sha256::new();
    for part in [check_spec_id, command] {
        digest.update(part.as_bytes());
        digest.update([0]);
    }
    for arg in args {
        digest.update(arg.as_bytes());
        digest.update([0]);
    }
    for part in [cwd, scope_hash, effect] {
        digest.update(part.as_bytes());
        digest.update([0]);
    }
    format!("sha256:{:x}", digest.finalize())
}

fn canonical_json_bytes(value: &serde_json::Value) -> Result<Vec<u8>> {
    let canonical = canonicalize_value(value)?;
    serde_json::to_vec(&canonical)
        .map_err(|error| anyhow!("failed to serialize canonical json: {error}"))
}

fn canonicalize_value(value: &serde_json::Value) -> Result<serde_json::Value> {
    match value {
        serde_json::Value::Array(values) => values
            .iter()
            .map(canonicalize_value)
            .collect::<Result<Vec<_>>>()
            .map(serde_json::Value::Array),
        serde_json::Value::Object(object) => {
            let ordered = object
                .iter()
                .map(|(key, value)| canonicalize_value(value).map(|value| (key.clone(), value)))
                .collect::<Result<BTreeMap<_, _>>>()?;
            Ok(serde_json::Value::Object(ordered.into_iter().collect()))
        }
        serde_json::Value::Number(number) => Ok(serde_json::Value::Number(number.clone())),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::String(_) => {
            Ok(value.clone())
        }
    }
}
