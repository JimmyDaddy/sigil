use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

use crate::UpdateChannel;

/// Owner of the installed `sigil` executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallSource {
    StandaloneGitHubArchive,
    Npm,
    Homebrew,
    Cargo,
    Source,
    Unknown,
}

impl InstallSource {
    #[must_use]
    pub const fn permits_binary_replacement(self) -> bool {
        matches!(self, Self::StandaloneGitHubArchive)
    }

    #[must_use]
    pub const fn permits_automatic_check(self) -> bool {
        matches!(
            self,
            Self::StandaloneGitHubArchive | Self::Npm | Self::Homebrew
        )
    }
}

/// Process environment facts that gate unsolicited network checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutomaticCheckEnvironment {
    pub ci: bool,
    pub disabled: bool,
}

impl AutomaticCheckEnvironment {
    #[must_use]
    pub fn current() -> Self {
        Self {
            ci: environment_flag("CI") || environment_flag("GITHUB_ACTIONS"),
            disabled: environment_flag("SIGIL_NO_UPDATE_CHECK"),
        }
    }
}

#[must_use]
pub fn automatic_check_allowed(
    profile: &str,
    source: InstallSource,
    environment: AutomaticCheckEnvironment,
) -> bool {
    profile == "release"
        && source.permits_automatic_check()
        && !environment.ci
        && !environment.disabled
}

/// Classifies an executable conservatively. Ambiguous paths never receive
/// in-place replacement authority.
#[must_use]
pub fn resolve_install_source(
    distribution: &str,
    current_exe: &Path,
    explicit_marker: Option<&str>,
) -> InstallSource {
    let compiled_source = distribution_source(distribution);
    if let Some(source) = explicit_marker
        .and_then(marker_source)
        .filter(|source| *source != InstallSource::StandaloneGitHubArchive)
    {
        return source;
    }
    if homebrew_cellar_path(current_exe) {
        return InstallSource::Homebrew;
    }
    if npm_package_path(current_exe) {
        return InstallSource::Npm;
    }
    if cargo_bin_path(current_exe) {
        return InstallSource::Cargo;
    }
    compiled_source
}

fn distribution_source(distribution: &str) -> InstallSource {
    match distribution.trim().to_ascii_lowercase().as_str() {
        "github-release" | "standalone" => InstallSource::StandaloneGitHubArchive,
        "npm" => InstallSource::Npm,
        "homebrew" => InstallSource::Homebrew,
        "cargo" => InstallSource::Cargo,
        "source" | "development" => InstallSource::Source,
        _ => InstallSource::Unknown,
    }
}

/// Returns the original package-manager command for externally managed installs.
#[must_use]
pub fn managed_update_command(
    source: InstallSource,
    _channel: UpdateChannel,
    version: &str,
) -> Option<String> {
    match source {
        InstallSource::Npm => {
            let version = semver::Version::parse(version).ok()?;
            let tag = if version.pre.is_empty() {
                "latest".to_owned()
            } else if version.pre.as_str().split('.').next() == Some("beta") {
                "beta".to_owned()
            } else {
                version.to_string()
            };
            Some(format!("npm install -g @sigil-ai/sigil@{tag}"))
        }
        InstallSource::Homebrew => Some("brew upgrade sigil-ai".to_owned()),
        InstallSource::Cargo => Some(format!(
            "cargo install --git https://github.com/JimmyDaddy/sigil --tag v{version} --locked sigil --force"
        )),
        InstallSource::StandaloneGitHubArchive | InstallSource::Source | InstallSource::Unknown => {
            None
        }
    }
}

fn marker_source(value: &str) -> Option<InstallSource> {
    match value.trim().to_ascii_lowercase().as_str() {
        "github-release" | "standalone" => Some(InstallSource::StandaloneGitHubArchive),
        "npm" => Some(InstallSource::Npm),
        "homebrew" => Some(InstallSource::Homebrew),
        "cargo" => Some(InstallSource::Cargo),
        "source" | "development" => Some(InstallSource::Source),
        _ => None,
    }
}

fn homebrew_cellar_path(path: &Path) -> bool {
    let components = normal_components(path);
    components
        .windows(3)
        .any(|window| window[0] == "Cellar" && window[1] == "sigil-ai")
}

fn cargo_bin_path(path: &Path) -> bool {
    let components = normal_components(path);
    components.windows(3).any(|window| {
        (window[0] == ".cargo" || window[0] == "cargo")
            && window[1] == "bin"
            && executable_name_matches(&window[2])
    })
}

fn npm_package_path(path: &Path) -> bool {
    let components = normal_components(path);
    components.windows(3).any(|window| {
        window[0] == "node_modules" && window[1] == "@sigil-ai" && window[2].starts_with("sigil-")
    })
}

fn executable_name_matches(name: &str) -> bool {
    name == "sigil" || name.eq_ignore_ascii_case("sigil.exe")
}

fn normal_components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

fn environment_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| {
        let value = value.trim();
        !value.is_empty()
            && !matches!(
                value.to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
    })
}

#[cfg(test)]
#[path = "tests/install_source_tests.rs"]
mod tests;
