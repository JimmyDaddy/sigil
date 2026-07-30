//! Shared update policy for Sigil product surfaces.
//!
//! This crate owns release discovery, integrity admission, install-source
//! classification, cache policy, and standalone binary replacement. It does
//! not own product UI, release publication, or package-manager execution.

mod apply;
mod cache;
mod channel;
mod github;
mod install_source;

use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use apply::{UpdateApplyOutcome, apply_checked_update};
pub use channel::UpdateChannel;
pub use github::{
    CheckOptions, ReleaseSecurity, UpdateCandidate, UpdateCheckOutcome, UpdateService,
};
pub use install_source::{
    AutomaticCheckEnvironment, InstallSource, automatic_check_allowed, managed_update_command,
    resolve_install_source,
};

/// Relative path below Sigil's global cache root used for update checks.
pub const UPDATE_CACHE_RELATIVE_PATH: &str = "updates/v1/check.json";

/// Build facts supplied by the final binary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildMetadata {
    pub version: String,
    pub target: String,
    pub profile: String,
    pub distribution: String,
}

impl BuildMetadata {
    #[must_use]
    pub fn new(
        version: impl Into<String>,
        target: impl Into<String>,
        profile: impl Into<String>,
        distribution: impl Into<String>,
    ) -> Self {
        Self {
            version: version.into(),
            target: target.into(),
            profile: profile.into(),
            distribution: distribution.into(),
        }
    }

    #[must_use]
    pub fn source(
        version: impl Into<String>,
        target: impl Into<String>,
        profile: impl Into<String>,
    ) -> Self {
        Self::new(version, target, profile, "source")
    }

    #[must_use]
    pub fn install_source(&self, current_exe: &Path) -> InstallSource {
        resolve_install_source(
            &self.distribution,
            current_exe,
            std::env::var("SIGIL_INSTALL_SOURCE").ok().as_deref(),
        )
    }
}

/// Stable update-domain failures shared by CLI and interactive surfaces.
#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("invalid update channel `{0}`")]
    InvalidChannel(String),
    #[error("invalid current version `{version}`: {source}")]
    InvalidCurrentVersion {
        version: String,
        source: semver::Error,
    },
    #[error("failed to build the update HTTP client: {0}")]
    HttpClient(reqwest::Error),
    #[error("GitHub release request failed: {0}")]
    Http(reqwest::Error),
    #[error("GitHub release request returned HTTP {0}")]
    HttpStatus(reqwest::StatusCode),
    #[error("GitHub release response exceeded {limit} bytes")]
    ResponseTooLarge { limit: usize },
    #[error("GitHub release response was invalid: {0}")]
    InvalidResponse(serde_json::Error),
    #[error("update cache operation failed: {0}")]
    Cache(String),
    #[error("no newer release is available")]
    NoUpdate,
    #[error("release cannot be installed safely: {0}")]
    SecurityBlocked(String),
    #[error("standalone update requires a regular executable path")]
    InvalidExecutable,
    #[error("update engine failed: {0}")]
    Engine(#[from] self_update::errors::Error),
}

pub type Result<T> = std::result::Result<T, UpdateError>;
