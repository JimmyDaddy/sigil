//! RFC-0071 section 11.1: reserved environment construction.
//!
//! Restricted execution writes the reserved keys (TMPDIR/TMP/TEMP, HOME, XDG_STATE_HOME,
//! XDG_CACHE_HOME, SIGIL_STATE_HOME, SIGIL_CACHE_HOME, SIGIL_SCRATCH_DIR) from the sandbox
//! service only. A command-local assignment cannot change the grant; overrides are recorded in
//! the environment receipt and the sandbox still only allows lease roots.

use std::collections::BTreeMap;
use std::path::Path;

/// Closed reserved environment key set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReservedEnvKeyV1 {
    Tmpdir,
    Tmp,
    Temp,
    Home,
    XdgStateHome,
    XdgCacheHome,
    SigilStateHome,
    SigilCacheHome,
    SigilScratchDir,
}

impl ReservedEnvKeyV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tmpdir => "TMPDIR",
            Self::Tmp => "TMP",
            Self::Temp => "TEMP",
            Self::Home => "HOME",
            Self::XdgStateHome => "XDG_STATE_HOME",
            Self::XdgCacheHome => "XDG_CACHE_HOME",
            Self::SigilStateHome => "SIGIL_STATE_HOME",
            Self::SigilCacheHome => "SIGIL_CACHE_HOME",
            Self::SigilScratchDir => "SIGIL_SCRATCH_DIR",
        }
    }

    /// The eight standard profile keys (SIGIL_SCRATCH_DIR is optional).
    pub const fn is_standard(self) -> bool {
        !matches!(self, Self::SigilScratchDir)
    }
}

/// Standard reserved environment: maps each key into the ExecutionTemp absolute layout.
pub fn standard_reserved_environment(execution_temp_root: &Path) -> BTreeMap<String, String> {
    use ReservedEnvKeyV1::*;
    let root = execution_temp_root;
    let mut env = BTreeMap::new();
    env.insert(
        Tmpdir.as_str().to_owned(),
        root.join("tmp").to_string_lossy().into_owned(),
    );
    env.insert(
        Tmp.as_str().to_owned(),
        root.join("tmp").to_string_lossy().into_owned(),
    );
    env.insert(
        Temp.as_str().to_owned(),
        root.join("tmp").to_string_lossy().into_owned(),
    );
    env.insert(
        Home.as_str().to_owned(),
        root.join("home").to_string_lossy().into_owned(),
    );
    env.insert(
        XdgStateHome.as_str().to_owned(),
        root.join("state").to_string_lossy().into_owned(),
    );
    env.insert(
        XdgCacheHome.as_str().to_owned(),
        root.join("cache").to_string_lossy().into_owned(),
    );
    env.insert(
        SigilStateHome.as_str().to_owned(),
        root.join("sigil-state").to_string_lossy().into_owned(),
    );
    env.insert(
        SigilCacheHome.as_str().to_owned(),
        root.join("sigil-cache").to_string_lossy().into_owned(),
    );
    // Windows process creation still needs the small OS loader/shell baseline after the
    // authority clears ambient variables. These names describe the platform installation, not
    // user credentials or writable state, and are the same bounded set used by the kernel's
    // isolated extension environment policy.
    #[cfg(windows)]
    for name in ["SystemRoot", "WINDIR", "ComSpec", "PATHEXT"] {
        if let Some(value) = std::env::var_os(name) {
            env.insert(name.to_owned(), value.to_string_lossy().into_owned());
        }
    }
    env
}

/// Closed override classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservedEnvOverrideV1 {
    None,
    OverrideAttempt,
    OverrideAcceptedUnconfined,
}

/// Applies the reserved environment onto a candidate, tagging override attempts.
///
/// Restricted profiles must reject reserved overrides; danger-full-access may accept but must
/// record the unconfined effective environment.
pub fn apply_reserved_environment(
    candidate: &mut BTreeMap<String, String>,
    standard: &BTreeMap<String, String>,
    session_scratch: Option<&Path>,
    allow_override: bool,
) -> (ReservedEnvOverrideV1, BTreeMap<String, String>) {
    let mut overrides = Vec::new();
    for (key, value) in standard {
        if candidate.get(key).is_some_and(|existing| existing != value) {
            overrides.push(key.clone());
        }
        candidate.insert(key.clone(), value.clone());
    }
    if let Some(scratch) = session_scratch {
        candidate.insert(
            ReservedEnvKeyV1::SigilScratchDir.as_str().to_owned(),
            scratch.to_string_lossy().into_owned(),
        );
    }
    let classification = if overrides.is_empty() {
        ReservedEnvOverrideV1::None
    } else if allow_override {
        ReservedEnvOverrideV1::OverrideAcceptedUnconfined
    } else {
        ReservedEnvOverrideV1::OverrideAttempt
    };
    (classification, candidate.clone())
}

/// Pathless execution-temp plan: no host path in public/deduped scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionTempEnvPlanV1 {
    pub layout_hash: String,
    pub standard_keys_present: bool,
    pub scratch_dir_present: bool,
    pub override_classification: ReservedEnvOverrideV1,
}

#[cfg(test)]
#[path = "tests/environment_tests.rs"]
mod tests;
