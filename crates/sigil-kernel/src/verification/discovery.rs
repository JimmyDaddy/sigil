use super::*;

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
        let command = check.normalized_command(&canonical_root)?;
        if !user_configured_check_applies_to_workspace(&canonical_root, &command) {
            continue;
        }
        let check_spec_id = unique_check_id(check.id.clone(), &mut used_ids);
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
        if !user_configured_check_applies_to_workspace(workspace_root, &command) {
            continue;
        }
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

pub(super) fn normalize_check_cwd(
    workspace_root: &Path,
    cwd: Option<&PathBuf>,
) -> Result<Option<PathBuf>> {
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

fn user_configured_check_applies_to_workspace(
    workspace_root: &Path,
    command: &CheckCommand,
) -> bool {
    let check_root = command.cwd.as_ref().map_or_else(
        || workspace_root.to_path_buf(),
        |cwd| workspace_root.join(cwd),
    );
    match project_marker_family(&command.command) {
        Some(ProjectMarkerFamily::Cargo) => {
            has_marker_in_workspace_chain(workspace_root, &check_root, &["Cargo.toml"])
        }
        Some(ProjectMarkerFamily::PackageJson) => {
            has_marker_in_workspace_chain(workspace_root, &check_root, &["package.json"])
        }
        Some(ProjectMarkerFamily::Makefile) => has_marker_in_workspace_chain(
            workspace_root,
            &check_root,
            &["Makefile", "makefile", "GNUmakefile"],
        ),
        Some(ProjectMarkerFamily::Justfile) => {
            has_marker_in_workspace_chain(workspace_root, &check_root, &["justfile", "Justfile"])
        }
        None => true,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectMarkerFamily {
    Cargo,
    PackageJson,
    Makefile,
    Justfile,
}

fn project_marker_family(command: &str) -> Option<ProjectMarkerFamily> {
    let program = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())?;
    match program {
        "cargo" => Some(ProjectMarkerFamily::Cargo),
        "npm" | "pnpm" | "yarn" | "bun" => Some(ProjectMarkerFamily::PackageJson),
        "make" | "gmake" => Some(ProjectMarkerFamily::Makefile),
        "just" => Some(ProjectMarkerFamily::Justfile),
        _ => None,
    }
}

fn has_marker_in_workspace_chain(workspace_root: &Path, start: &Path, markers: &[&str]) -> bool {
    let mut current = start;
    loop {
        if markers.iter().any(|marker| current.join(marker).is_file()) {
            return true;
        }
        if current == workspace_root {
            return false;
        }
        let Some(parent) = current.parent() else {
            return false;
        };
        if !parent.starts_with(workspace_root) {
            return false;
        }
        current = parent;
    }
}
