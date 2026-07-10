use std::{
    collections::{BTreeMap, BTreeSet},
    env, fmt,
    sync::OnceLock,
};

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroizing;

/// Versioned identity for the extension-process environment baseline and grant semantics.
pub const EXTENSION_ENVIRONMENT_POLICY_VERSION: &str = "isolated_extension_v1";

const EXTENSION_BASELINE_ENVIRONMENT_NAMES: &[&str] = &[
    "PATH", "LANG", "LC_ALL", "LC_CTYPE", "TZ", "TMPDIR", "TMP", "TEMP",
];

#[cfg(windows)]
const WINDOWS_EXTENSION_BASELINE_ENVIRONMENT_NAMES: &[&str] =
    &["SystemRoot", "WINDIR", "ComSpec", "PATHEXT"];

static PROCESS_ENVIRONMENT_FINGERPRINT_KEY: OnceLock<[u8; 32]> = OnceLock::new();

/// Whether a process receives the parent environment or a replacement extension environment.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessEnvironmentPolicy {
    /// Preserve the existing user-command environment overlay behavior.
    #[default]
    InheritParent,
    /// Clear the parent environment and inject only the resolved extension baseline and grants.
    IsolatedExtension,
}

/// Whether an extension lifecycle outcome happened before or after a child process was spawned.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionProcessLaunchPhase {
    PreSpawn,
    PostSpawn,
}

/// Stable terminal state for one extension process startup attempt.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionProcessLifecycleStatus {
    Registered,
    StartupFailed,
    ToolsListFailed,
}

/// Secret-free durable audit payload for one extension process startup outcome.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ExtensionProcessLifecycleAudit {
    pub process_kind: String,
    pub subject: String,
    pub phase: ExtensionProcessLaunchPhase,
    pub status: ExtensionProcessLifecycleStatus,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub safe_metadata: BTreeMap<String, String>,
}

impl ProcessEnvironmentPolicy {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InheritParent => "inherit_parent",
            Self::IsolatedExtension => "isolated_extension",
        }
    }

    #[must_use]
    pub fn clears_parent(self) -> bool {
        self == Self::IsolatedExtension
    }
}

/// Stable typed failure codes for extension process preflight and live binding checks.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionProcessLaunchErrorCode {
    ConfigurationInvalid,
    EnvironmentBindingChanged,
    NetworkIsolationUnavailable,
    ProcessIsolationUnavailable,
    BackendReceiptInvalid,
}

impl ExtensionProcessLaunchErrorCode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ConfigurationInvalid => "configuration_invalid",
            Self::EnvironmentBindingChanged => "environment_binding_changed",
            Self::NetworkIsolationUnavailable => "network_isolation_unavailable",
            Self::ProcessIsolationUnavailable => "process_isolation_unavailable",
            Self::BackendReceiptInvalid => "backend_receipt_invalid",
        }
    }
}

impl fmt::Display for ExtensionProcessLaunchErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Structured extension process failure that never carries a resolved environment value.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("{code}: {message}")]
pub struct ExtensionProcessLaunchError {
    pub code: ExtensionProcessLaunchErrorCode,
    pub subject: String,
    pub message: String,
}

impl ExtensionProcessLaunchError {
    #[must_use]
    pub fn configuration_invalid(subject: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: ExtensionProcessLaunchErrorCode::ConfigurationInvalid,
            subject: subject.into(),
            message: message.into(),
        }
    }

    #[must_use]
    pub fn environment_binding_changed(
        subject: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: ExtensionProcessLaunchErrorCode::EnvironmentBindingChanged,
            subject: subject.into(),
            message: message.into(),
        }
    }

    #[must_use]
    pub fn isolation_unavailable(
        code: ExtensionProcessLaunchErrorCode,
        subject: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            subject: subject.into(),
            message: message.into(),
        }
    }
}

/// Secret environment value with redacted `Debug`, no serde implementation, and zeroizing drop.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(Zeroizing<String>);

impl SecretString {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    #[must_use]
    pub fn expose_secret(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretString([redacted])")
    }
}

/// Fully resolved replacement environment for one extension process launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProcessEnvironment {
    policy: ProcessEnvironmentPolicy,
    variables: BTreeMap<String, SecretString>,
    baseline_names: Vec<String>,
    grant_names: Vec<String>,
    static_fingerprint: String,
    live_fingerprint: String,
}

impl ResolvedProcessEnvironment {
    #[must_use]
    pub fn policy(&self) -> ProcessEnvironmentPolicy {
        self.policy
    }

    pub fn variables(&self) -> impl Iterator<Item = (&str, &SecretString)> {
        self.variables
            .iter()
            .map(|(name, value)| (name.as_str(), value))
    }

    /// Returns one resolved baseline or grant value by its environment variable name.
    #[must_use]
    pub fn variable(&self, name: &str) -> Option<&SecretString> {
        self.variables.get(name)
    }

    #[must_use]
    pub fn baseline_names(&self) -> &[String] {
        &self.baseline_names
    }

    #[must_use]
    pub fn grant_names(&self) -> &[String] {
        &self.grant_names
    }

    #[must_use]
    pub fn static_fingerprint(&self) -> &str {
        &self.static_fingerprint
    }

    #[must_use]
    pub fn live_fingerprint(&self) -> &str {
        &self.live_fingerprint
    }
}

/// Validates, de-duplicates, and sorts environment variable names.
///
/// # Errors
///
/// Returns [`ExtensionProcessLaunchErrorCode::ConfigurationInvalid`] when any name does not match
/// `[A-Za-z_][A-Za-z0-9_]*`.
pub fn normalize_environment_variable_names(
    names: &[String],
) -> Result<Vec<String>, ExtensionProcessLaunchError> {
    let mut normalized = BTreeSet::new();
    for name in names {
        if !valid_environment_variable_name(name) {
            return Err(ExtensionProcessLaunchError::configuration_invalid(
                name,
                format!("environment grant name {name:?} must match [A-Za-z_][A-Za-z0-9_]*"),
            ));
        }
        normalized.insert(name.clone());
    }
    Ok(normalized.into_iter().collect())
}

/// Computes the non-secret policy and grant-name fingerprint used by trust and pin checks.
///
/// # Errors
///
/// Returns a configuration error when a grant name is invalid or the fingerprint material cannot
/// be encoded.
pub fn extension_environment_static_fingerprint(
    grant_names: &[String],
) -> Result<String, ExtensionProcessLaunchError> {
    let grant_names = normalize_environment_variable_names(grant_names)?;
    let encoded = serde_json::to_vec(&json!({
        "policy": EXTENSION_ENVIRONMENT_POLICY_VERSION,
        "source": "parent_environment",
        "grant_names": grant_names,
    }))
    .map_err(|error| {
        ExtensionProcessLaunchError::configuration_invalid(
            "inherit_env",
            format!("failed to encode environment fingerprint material: {error}"),
        )
    })?;
    Ok(format!("sha256:{:x}", Sha256::digest(encoded)))
}

/// Resolves only the fixed baseline and explicitly granted parent variables before process spawn.
///
/// # Errors
///
/// Returns a configuration error when a grant name is invalid, a granted variable is missing or
/// non-UTF-8, or fingerprint material cannot be encoded.
pub fn resolve_extension_process_environment(
    grant_names: &[String],
) -> Result<ResolvedProcessEnvironment, ExtensionProcessLaunchError> {
    resolve_extension_process_environment_with(
        grant_names,
        |name| Ok(env::var_os(name).map(|value| value.to_string_lossy().into_owned())),
        |name| match env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(env::VarError::NotPresent) => Ok(None),
            Err(env::VarError::NotUnicode(_)) => {
                Err(ExtensionProcessLaunchError::configuration_invalid(
                    name,
                    format!("environment grant {name} is not valid UTF-8"),
                ))
            }
        },
        process_environment_fingerprint_key(),
    )
}

fn resolve_extension_process_environment_with<BaselineLookup, GrantLookup>(
    grant_names: &[String],
    mut baseline_lookup: BaselineLookup,
    mut grant_lookup: GrantLookup,
    fingerprint_key: &[u8; 32],
) -> Result<ResolvedProcessEnvironment, ExtensionProcessLaunchError>
where
    BaselineLookup: FnMut(&str) -> Result<Option<String>, ExtensionProcessLaunchError>,
    GrantLookup: FnMut(&str) -> Result<Option<String>, ExtensionProcessLaunchError>,
{
    let grant_names = normalize_environment_variable_names(grant_names)?;
    let static_fingerprint = extension_environment_static_fingerprint(&grant_names)?;
    let mut variables = BTreeMap::new();
    let mut baseline_names = Vec::new();
    for name in extension_baseline_environment_names() {
        let value = baseline_lookup(name)?
            .or_else(|| (name == "PATH").then(|| default_extension_path().to_owned()));
        if let Some(value) = value {
            baseline_names.push(name.to_owned());
            variables.insert(name.to_owned(), SecretString::new(value));
        }
    }

    let mut missing = Vec::new();
    for name in &grant_names {
        match grant_lookup(name)? {
            Some(value) => {
                variables.insert(name.clone(), SecretString::new(value));
            }
            None => missing.push(name.clone()),
        }
    }
    if !missing.is_empty() {
        return Err(ExtensionProcessLaunchError::configuration_invalid(
            "inherit_env",
            format!(
                "missing inherited environment variables: {}",
                missing.join(", ")
            ),
        ));
    }

    let live_fingerprint = environment_live_fingerprint(
        fingerprint_key,
        &static_fingerprint,
        &baseline_names,
        &grant_names,
        &variables,
    );
    Ok(ResolvedProcessEnvironment {
        policy: ProcessEnvironmentPolicy::IsolatedExtension,
        variables,
        baseline_names,
        grant_names,
        static_fingerprint,
        live_fingerprint,
    })
}

#[cfg(not(windows))]
fn extension_baseline_environment_names() -> Vec<&'static str> {
    EXTENSION_BASELINE_ENVIRONMENT_NAMES.to_vec()
}

#[cfg(windows)]
fn extension_baseline_environment_names() -> Vec<&'static str> {
    let mut names = EXTENSION_BASELINE_ENVIRONMENT_NAMES.to_vec();
    names.extend_from_slice(WINDOWS_EXTENSION_BASELINE_ENVIRONMENT_NAMES);
    names
}

fn default_extension_path() -> &'static str {
    if cfg!(windows) {
        r"C:\Windows\System32;C:\Windows"
    } else {
        "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
    }
}

fn valid_environment_variable_name(name: &str) -> bool {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn process_environment_fingerprint_key() -> &'static [u8; 32] {
    PROCESS_ENVIRONMENT_FINGERPRINT_KEY.get_or_init(|| {
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let mut key = [0_u8; 32];
        key[..16].copy_from_slice(first.as_bytes());
        key[16..].copy_from_slice(second.as_bytes());
        key
    })
}

fn environment_live_fingerprint(
    key: &[u8; 32],
    static_fingerprint: &str,
    baseline_names: &[String],
    grant_names: &[String],
    variables: &BTreeMap<String, SecretString>,
) -> String {
    let mut inner = Sha256::new();
    let mut inner_pad = [0x36_u8; 64];
    let mut outer_pad = [0x5c_u8; 64];
    for (index, byte) in key.iter().enumerate() {
        inner_pad[index] ^= byte;
        outer_pad[index] ^= byte;
    }
    inner.update(inner_pad);
    update_fingerprint_part(&mut inner, "static", static_fingerprint);
    for name in baseline_names {
        if let Some(value) = variables.get(name) {
            update_fingerprint_part(&mut inner, "baseline_name", name);
            update_fingerprint_part(&mut inner, "baseline_value", value.expose_secret());
        }
    }
    for name in grant_names {
        if let Some(value) = variables.get(name) {
            update_fingerprint_part(&mut inner, "grant_source", "parent_environment");
            update_fingerprint_part(&mut inner, "grant_name", name);
            update_fingerprint_part(&mut inner, "grant_value", value.expose_secret());
        }
    }
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    format!("hmac-sha256:{:x}", outer.finalize())
}

fn update_fingerprint_part(hasher: &mut Sha256, label: &str, value: &str) {
    hasher.update((label.len() as u64).to_be_bytes());
    hasher.update(label.as_bytes());
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
}

#[cfg(test)]
#[path = "tests/process_environment_tests.rs"]
mod tests;
