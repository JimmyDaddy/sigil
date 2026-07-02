use super::*;

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
