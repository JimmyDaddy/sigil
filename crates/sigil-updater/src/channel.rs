use std::{fmt, str::FromStr};

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::UpdateError;

/// Release channel used when choosing a newer semantic version.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateChannel {
    /// Follow stable releases, or the installed version's exact prerelease channel.
    #[default]
    Current,
    /// Select only non-prerelease versions.
    Stable,
    /// Select the newest prerelease or stable version.
    Beta,
}

impl UpdateChannel {
    #[must_use]
    pub(crate) fn accepts(self, current: &Version, candidate: &Version, prerelease: bool) -> bool {
        match self {
            Self::Stable => !prerelease && candidate.pre.is_empty(),
            Self::Beta => candidate.pre.is_empty() || prerelease_channel(candidate) == Some("beta"),
            Self::Current if current.pre.is_empty() => !prerelease && candidate.pre.is_empty(),
            Self::Current => {
                candidate.pre.is_empty()
                    || prerelease_channel(candidate) == prerelease_channel(current)
            }
        }
    }
}

fn prerelease_channel(version: &Version) -> Option<&str> {
    version
        .pre
        .as_str()
        .split('.')
        .next()
        .filter(|part| !part.is_empty())
}

impl fmt::Display for UpdateChannel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Current => "current",
            Self::Stable => "stable",
            Self::Beta => "beta",
        })
    }
}

impl FromStr for UpdateChannel {
    type Err = UpdateError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "current" => Ok(Self::Current),
            "stable" => Ok(Self::Stable),
            "beta" => Ok(Self::Beta),
            _ => Err(UpdateError::InvalidChannel(value.to_owned())),
        }
    }
}

#[cfg(test)]
#[path = "tests/channel_tests.rs"]
mod tests;
