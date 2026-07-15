use std::{
    env,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use sigil_kernel::{SecretRedactor, safe_persistence_text};

use crate::doctor::{DoctorCheck, DoctorReport, DoctorStatus};

pub const DOCTOR_SUPPORT_SCHEMA_VERSION: u32 = 1;
pub const MAX_DOCTOR_SUPPORT_CHECKS: usize = 256;
pub const MAX_DOCTOR_SUPPORT_NAME_BYTES: usize = 256;
pub const MAX_DOCTOR_SUPPORT_TEXT_BYTES: usize = 2_048;
pub const MAX_DOCTOR_SUPPORT_JSON_BYTES: usize = 256 * 1_024;
pub const SUPPORT_BUNDLE_SCHEMA_VERSION: u32 = 1;
pub const MAX_SUPPORT_BUNDLE_JSON_BYTES: usize = 384 * 1_024;
pub const SUPPORT_BUNDLES_DIRECTORY_NAME: &str = "support-bundles";

const INCLUDED_CATEGORIES: &[&str] = &[
    "build_metadata",
    "os_arch",
    "terminal_family",
    "doctor_status_and_redacted_checks",
    "provider_and_model_labels",
    "mcp_aliases",
    "credential_environment_variable_names",
    "capability_and_sandbox_status",
];
const EXCLUDED_CATEGORIES: &[&str] = &[
    "conversation_content",
    "tool_input_output",
    "file_content_and_diff",
    "config_file_content",
    "credential_and_environment_values",
    "local_paths_and_private_endpoints",
    "session_log_content",
];

/// Build identity supplied by the final `sigil` binary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportBuildInfo {
    pub version: String,
    pub commit: String,
    pub target: String,
    pub profile: String,
}

impl SupportBuildInfo {
    #[must_use]
    pub fn new(
        version: impl Into<String>,
        commit: impl Into<String>,
        target: impl Into<String>,
        profile: impl Into<String>,
    ) -> Self {
        Self {
            version: version.into(),
            commit: commit.into(),
            target: target.into(),
            profile: profile.into(),
        }
    }

    #[must_use]
    pub fn unknown() -> Self {
        Self::new("unknown", "unknown", "unknown", "unknown")
    }
}

/// Coarse terminal family. Raw terminal environment values are never serialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportTerminalFamily {
    Iterm2,
    AppleTerminal,
    Wezterm,
    Vscode,
    Other,
    Unknown,
}

/// Non-secret platform facts included in a support report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportEnvironmentV1 {
    pub os: String,
    pub architecture: String,
    pub terminal_family: SupportTerminalFamily,
}

impl SupportEnvironmentV1 {
    #[must_use]
    pub fn current() -> Self {
        Self::from_terminal_values(
            env::consts::OS,
            env::consts::ARCH,
            env::var("TERM_PROGRAM").ok().as_deref(),
            env::var("TERM").ok().as_deref(),
        )
    }

    #[must_use]
    pub fn from_terminal_values(
        os: impl Into<String>,
        architecture: impl Into<String>,
        term_program: Option<&str>,
        term: Option<&str>,
    ) -> Self {
        Self {
            os: os.into(),
            architecture: architecture.into(),
            terminal_family: terminal_family(term_program, term),
        }
    }
}

/// Stable status tokens for the V1 support schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportDoctorStatus {
    Ok,
    Warn,
    Error,
}

impl From<DoctorStatus> for SupportDoctorStatus {
    fn from(value: DoctorStatus) -> Self {
        match value {
            DoctorStatus::Ok => Self::Ok,
            DoctorStatus::Warn => Self::Warn,
            DoctorStatus::Error => Self::Error,
        }
    }
}

/// Count summary kept separate from human-readable doctor details.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportDoctorSummaryV1 {
    pub overall_status: SupportDoctorStatus,
    pub ok: usize,
    pub warn: usize,
    pub error: usize,
}

/// One allowlisted and redacted doctor check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportDoctorCheckV1 {
    pub status: SupportDoctorStatus,
    pub name: String,
    pub summary: String,
    pub remediation: Option<String>,
}

/// Explicit privacy projection included in every report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportPrivacyV1 {
    pub included: Vec<String>,
    pub excluded: Vec<String>,
    pub review_before_sharing: bool,
}

/// Frozen JSON contract emitted by `sigil doctor --output json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoctorSupportReportV1 {
    pub schema_version: u32,
    pub generated_at_unix_ms: u64,
    pub build: SupportBuildInfo,
    pub environment: SupportEnvironmentV1,
    pub summary: SupportDoctorSummaryV1,
    pub checks: Vec<SupportDoctorCheckV1>,
    pub privacy: SupportPrivacyV1,
}

impl DoctorSupportReportV1 {
    pub fn to_pretty_json(&self) -> Result<String> {
        let json = serde_json::to_string_pretty(self)?;
        if json.len() > MAX_DOCTOR_SUPPORT_JSON_BYTES {
            bail!(
                "doctor support JSON is {} bytes; maximum is {MAX_DOCTOR_SUPPORT_JSON_BYTES}",
                json.len()
            );
        }
        Ok(json)
    }
}

/// Stable coarse run phases included in a support bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportRunPhase {
    Idle,
    Thinking,
    Agent,
    Tool,
    Streaming,
}

/// Bounded session metadata. Conversation and session-log content are excluded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportSessionSummaryV1 {
    session_id: String,
    durable_entry_count: usize,
    provider: String,
    model: String,
    run_phase: SupportRunPhase,
    busy: bool,
}

impl SupportSessionSummaryV1 {
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    #[must_use]
    pub fn durable_entry_count(&self) -> usize {
        self.durable_entry_count
    }

    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    #[must_use]
    pub fn run_phase(&self) -> SupportRunPhase {
        self.run_phase
    }

    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.busy
    }
}

/// Non-serializable inputs used to sanitize coarse session labels.
pub struct SupportSessionProjectionContext<'a> {
    pub redactor: &'a SecretRedactor,
    pub path_redactions: &'a [SupportPathRedaction],
}

/// Produces the bounded session portion without reading conversation or log content.
pub fn project_support_session_summary_v1(
    session_id: &str,
    durable_entry_count: usize,
    provider: &str,
    model: &str,
    run_phase: SupportRunPhase,
    busy: bool,
    context: SupportSessionProjectionContext<'_>,
) -> Result<SupportSessionSummaryV1> {
    let projected = SupportSessionSummaryV1 {
        session_id: sanitize_field(
            "session.session_id",
            session_id,
            MAX_DOCTOR_SUPPORT_NAME_BYTES,
            context.redactor,
            context.path_redactions,
        )?,
        durable_entry_count,
        provider: sanitize_field(
            "session.provider",
            provider,
            MAX_DOCTOR_SUPPORT_NAME_BYTES,
            context.redactor,
            context.path_redactions,
        )?,
        model: sanitize_field(
            "session.model",
            model,
            MAX_DOCTOR_SUPPORT_NAME_BYTES,
            context.redactor,
            context.path_redactions,
        )?,
        run_phase,
        busy,
    };
    validate_support_session(&projected)?;
    Ok(projected)
}

/// Frozen private support bundle exported only after explicit TUI confirmation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportBundleV1 {
    pub schema_version: u32,
    pub doctor: DoctorSupportReportV1,
    pub session: Option<SupportSessionSummaryV1>,
}

impl SupportBundleV1 {
    #[must_use]
    pub fn new(doctor: DoctorSupportReportV1, session: Option<SupportSessionSummaryV1>) -> Self {
        Self {
            schema_version: SUPPORT_BUNDLE_SCHEMA_VERSION,
            doctor,
            session,
        }
    }

    pub fn to_pretty_json(&self) -> Result<String> {
        if self.schema_version != SUPPORT_BUNDLE_SCHEMA_VERSION {
            bail!(
                "support bundle schema version {} is unsupported",
                self.schema_version
            );
        }
        self.doctor.to_pretty_json()?;
        if let Some(session) = &self.session {
            validate_support_session(session)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        if json.len() > MAX_SUPPORT_BUNDLE_JSON_BYTES {
            bail!(
                "support bundle JSON is {} bytes; maximum is {MAX_SUPPORT_BUNDLE_JSON_BYTES}",
                json.len()
            );
        }
        Ok(json)
    }
}

/// Writes a private support bundle below the Sigil cache with restrictive permissions.
///
/// The destination name is generated internally, existing files are never replaced, and
/// serialization completes before any filesystem mutation.
pub fn write_support_bundle(cache_root: &Path, bundle: &SupportBundleV1) -> Result<PathBuf> {
    let json = bundle.to_pretty_json()?;
    let cache_root = prepare_private_directory(cache_root, "support cache")?;
    let support_dir = cache_root.join(SUPPORT_BUNDLES_DIRECTORY_NAME);
    reject_symlink(&support_dir, "support bundle directory")?;
    let support_dir = prepare_private_directory(&support_dir, "support bundle directory")?;
    if support_dir.parent() != Some(cache_root.as_path()) {
        bail!("support bundle directory escaped the support cache");
    }

    let suffix = uuid::Uuid::new_v4();
    let timestamp = bundle.doctor.generated_at_unix_ms;
    let destination = support_dir.join(format!("sigil-support-{timestamp}-{suffix}.json"));
    let temporary = support_dir.join(format!(".sigil-support-{timestamp}-{suffix}.tmp"));
    let mut published = false;
    let result = write_new_private_file(&temporary, json.as_bytes()).and_then(|()| {
        fs::hard_link(&temporary, &destination)?;
        published = true;
        fs::remove_file(&temporary)?;
        sync_directory(&support_dir)?;
        destination.canonicalize().map_err(Into::into)
    });
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
        if published {
            let _ = fs::remove_file(&destination);
        }
        let _ = sync_directory(&support_dir);
    }
    result
}

fn validate_support_session(session: &SupportSessionSummaryV1) -> Result<()> {
    validate_field(
        "session.session_id",
        &session.session_id,
        MAX_DOCTOR_SUPPORT_NAME_BYTES,
    )?;
    validate_field(
        "session.provider",
        &session.provider,
        MAX_DOCTOR_SUPPORT_NAME_BYTES,
    )?;
    validate_field(
        "session.model",
        &session.model,
        MAX_DOCTOR_SUPPORT_NAME_BYTES,
    )
}

fn prepare_private_directory(path: &Path, label: &str) -> Result<PathBuf> {
    reject_symlink(path, label)?;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(path)?;
    reject_symlink(path, label)?;
    if !path.is_dir() {
        bail!("{label} is not a directory");
    }
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    path.canonicalize().map_err(Into::into)
}

fn reject_symlink(path: &Path, label: &str) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("{label} must not be a symbolic link")
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn write_new_private_file(path: &Path, contents: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    fs::File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

/// Stable placeholders for caller-known private paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportPathKind {
    Config,
    Workspace,
    Cache,
    State,
    Home,
}

impl SupportPathKind {
    fn placeholder(self) -> &'static str {
        match self {
            Self::Config => "<config_path>",
            Self::Workspace => "<workspace_path>",
            Self::Cache => "<cache_path>",
            Self::State => "<state_path>",
            Self::Home => "<home_path>",
        }
    }
}

/// One exact path supplied only for redaction, never serialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupportPathRedaction {
    path: PathBuf,
    kind: SupportPathKind,
}

impl SupportPathRedaction {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, kind: SupportPathKind) -> Self {
        Self {
            path: path.into(),
            kind,
        }
    }
}

/// Non-serializable inputs required to produce one deterministic V1 projection.
pub struct DoctorSupportProjectionContext<'a> {
    pub generated_at_unix_ms: u64,
    pub build: &'a SupportBuildInfo,
    pub environment: &'a SupportEnvironmentV1,
    pub redactor: &'a SecretRedactor,
    pub path_redactions: &'a [SupportPathRedaction],
}

/// Projects an offline doctor report through a category allowlist and privacy boundary.
pub fn project_doctor_support_report_v1(
    report: &DoctorReport,
    context: DoctorSupportProjectionContext<'_>,
) -> Result<DoctorSupportReportV1> {
    if report.checks.len() > MAX_DOCTOR_SUPPORT_CHECKS {
        bail!(
            "doctor report has {} checks; maximum is {MAX_DOCTOR_SUPPORT_CHECKS}",
            report.checks.len()
        );
    }
    validate_build_info(context.build)?;
    validate_environment(context.environment)?;

    let mut counts = [0usize; 3];
    let checks = report
        .checks
        .iter()
        .map(|check| {
            match check.status {
                DoctorStatus::Ok => counts[0] += 1,
                DoctorStatus::Warn => counts[1] += 1,
                DoctorStatus::Error => counts[2] += 1,
            }
            project_check(check, &context)
        })
        .collect::<Result<Vec<_>>>()?;
    let projected = DoctorSupportReportV1 {
        schema_version: DOCTOR_SUPPORT_SCHEMA_VERSION,
        generated_at_unix_ms: context.generated_at_unix_ms,
        build: context.build.clone(),
        environment: context.environment.clone(),
        summary: SupportDoctorSummaryV1 {
            overall_status: report.overall_status().into(),
            ok: counts[0],
            warn: counts[1],
            error: counts[2],
        },
        checks,
        privacy: SupportPrivacyV1 {
            included: INCLUDED_CATEGORIES
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            excluded: EXCLUDED_CATEGORIES
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            review_before_sharing: true,
        },
    };
    projected.to_pretty_json()?;
    Ok(projected)
}

fn validate_build_info(build: &SupportBuildInfo) -> Result<()> {
    validate_field(
        "build.version",
        &build.version,
        MAX_DOCTOR_SUPPORT_NAME_BYTES,
    )?;
    validate_field("build.commit", &build.commit, MAX_DOCTOR_SUPPORT_NAME_BYTES)?;
    validate_field("build.target", &build.target, MAX_DOCTOR_SUPPORT_NAME_BYTES)?;
    validate_field(
        "build.profile",
        &build.profile,
        MAX_DOCTOR_SUPPORT_NAME_BYTES,
    )
}

fn validate_environment(environment: &SupportEnvironmentV1) -> Result<()> {
    validate_field(
        "environment.os",
        &environment.os,
        MAX_DOCTOR_SUPPORT_NAME_BYTES,
    )?;
    validate_field(
        "environment.architecture",
        &environment.architecture,
        MAX_DOCTOR_SUPPORT_NAME_BYTES,
    )
}

fn project_check(
    check: &DoctorCheck,
    context: &DoctorSupportProjectionContext<'_>,
) -> Result<SupportDoctorCheckV1> {
    let category = check.name.split(':').next().unwrap_or_default();
    if category == "terminal" {
        return Ok(SupportDoctorCheckV1 {
            status: check.status.into(),
            name: sanitize_field(
                "check.name",
                &check.name,
                MAX_DOCTOR_SUPPORT_NAME_BYTES,
                context.redactor,
                context.path_redactions,
            )?,
            summary: match check.status {
                DoctorStatus::Ok => "terminal compatibility check passed",
                DoctorStatus::Warn => "terminal compatibility check needs attention",
                DoctorStatus::Error => "terminal compatibility check failed",
            }
            .to_owned(),
            remediation: (check.status != DoctorStatus::Ok).then(|| {
                "review terminal compatibility settings in the Sigil troubleshooting guide"
                    .to_owned()
            }),
        });
    }
    if !allowlisted_category(category) {
        return Ok(SupportDoctorCheckV1 {
            status: check.status.into(),
            name: "other".to_owned(),
            summary: "details omitted for an unrecognized doctor category".to_owned(),
            remediation: None,
        });
    }
    Ok(SupportDoctorCheckV1 {
        status: check.status.into(),
        name: sanitize_field(
            "check.name",
            &check.name,
            MAX_DOCTOR_SUPPORT_NAME_BYTES,
            context.redactor,
            context.path_redactions,
        )?,
        summary: sanitize_field(
            "check.summary",
            &check.message,
            MAX_DOCTOR_SUPPORT_TEXT_BYTES,
            context.redactor,
            context.path_redactions,
        )?,
        remediation: check
            .remediation
            .as_deref()
            .map(|value| {
                sanitize_field(
                    "check.remediation",
                    value,
                    MAX_DOCTOR_SUPPORT_TEXT_BYTES,
                    context.redactor,
                    context.path_redactions,
                )
            })
            .transpose()?,
    })
}

fn allowlisted_category(category: &str) -> bool {
    matches!(
        category,
        "appearance"
            | "code_intelligence"
            | "compaction"
            | "config"
            | "execution"
            | "mcp"
            | "plugins"
            | "provider"
            | "session"
            | "storage"
            | "web"
            | "workspace"
    )
}

fn sanitize_field(
    field: &str,
    value: &str,
    max_bytes: usize,
    redactor: &SecretRedactor,
    path_redactions: &[SupportPathRedaction],
) -> Result<String> {
    let redacted = redactor.redact_text(value);
    let mut safe = safe_persistence_text(&redacted);
    let mut replacements = path_redactions
        .iter()
        .flat_map(path_replacement_variants)
        .filter(|(path, _)| !path.is_empty())
        .collect::<Vec<_>>();
    replacements.sort_by(|left, right| right.0.len().cmp(&left.0.len()));
    for (path, placeholder) in replacements {
        safe = safe.replace(&path, placeholder);
    }
    safe = redact_private_tokens(&safe);
    validate_field(field, &safe, max_bytes)?;
    Ok(safe)
}

fn path_replacement_variants(redaction: &SupportPathRedaction) -> Vec<(String, &'static str)> {
    let rendered = redaction.path.to_string_lossy().into_owned();
    let forward = rendered.replace('\\', "/");
    let backward = rendered.replace('/', "\\");
    let mut variants = vec![(rendered, redaction.kind.placeholder())];
    if variants[0].0 != forward {
        variants.push((forward, redaction.kind.placeholder()));
    }
    if variants.iter().all(|(value, _)| value != &backward) {
        variants.push((backward, redaction.kind.placeholder()));
    }
    variants
}

fn redact_private_tokens(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for segment in value.split_inclusive(char::is_whitespace) {
        let token_len = segment.trim_end_matches(char::is_whitespace).len();
        let (token, whitespace) = segment.split_at(token_len);
        let replacement = if token.contains("://") {
            "<endpoint>"
        } else if contains_absolute_path(token) {
            "<path>"
        } else {
            token
        };
        output.push_str(replacement);
        output.push_str(whitespace);
    }
    output
}

fn contains_absolute_path(token: &str) -> bool {
    let candidate = token.trim_matches(|character: char| {
        matches!(
            character,
            '`' | '\'' | '"' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';'
        )
    });
    let candidate = candidate
        .rsplit_once('=')
        .map_or(candidate, |(_, value)| value);
    candidate.starts_with('/')
        || candidate.starts_with("~/")
        || candidate.starts_with("\\\\")
        || candidate.as_bytes().get(1) == Some(&b':')
            && candidate
                .as_bytes()
                .get(2)
                .is_some_and(|separator| matches!(separator, b'\\' | b'/'))
}

fn validate_field(field: &str, value: &str, max_bytes: usize) -> Result<()> {
    if value.is_empty() {
        bail!("{field} must not be empty");
    }
    if value.len() > max_bytes {
        bail!("{field} is {} bytes; maximum is {max_bytes}", value.len());
    }
    Ok(())
}

fn terminal_family(term_program: Option<&str>, term: Option<&str>) -> SupportTerminalFamily {
    let value = term_program
        .or(term)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if value.is_empty() {
        return SupportTerminalFamily::Unknown;
    }
    if value.contains("iterm") {
        SupportTerminalFamily::Iterm2
    } else if value.contains("apple_terminal") || value.contains("apple terminal") {
        SupportTerminalFamily::AppleTerminal
    } else if value.contains("wezterm") {
        SupportTerminalFamily::Wezterm
    } else if value.contains("vscode") || value.contains("visual studio code") {
        SupportTerminalFamily::Vscode
    } else {
        SupportTerminalFamily::Other
    }
}

#[cfg(test)]
#[path = "tests/support_tests.rs"]
mod tests;
