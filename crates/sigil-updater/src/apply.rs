use std::{path::Path, process::Command};

use serde::{Deserialize, Serialize};

use crate::{InstallSource, Result, UpdateCheckOutcome, UpdateError, managed_update_command};

/// Terminal result of an explicit update apply request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum UpdateApplyOutcome {
    Installed { version: String },
    ManagedExternally { command: String },
}

/// Applies a previously checked update.
///
/// Only immutable GitHub standalone archives with an exact SHA-256 digest may
/// replace the executable. Package-manager installations return their original
/// update command and are never modified in place.
///
/// # Errors
///
/// Returns an error when no update exists, release security admission fails,
/// the executable is not a regular file, binary verification fails, or the
/// replacement engine fails.
pub async fn apply_checked_update(
    check: &UpdateCheckOutcome,
    current_exe: &Path,
) -> Result<UpdateApplyOutcome> {
    let candidate = check.candidate.as_ref().ok_or(UpdateError::NoUpdate)?;
    if check.install_source != InstallSource::StandaloneGitHubArchive {
        let command = check
            .managed_update_command
            .clone()
            .or_else(|| {
                managed_update_command(check.install_source, check.channel, &candidate.version)
            })
            .ok_or_else(|| {
                UpdateError::SecurityBlocked(
                    "this installation source cannot be updated in place".to_owned(),
                )
            })?;
        return Ok(UpdateApplyOutcome::ManagedExternally { command });
    }
    if !candidate.security.eligible_for_apply {
        return Err(UpdateError::SecurityBlocked(
            candidate
                .security
                .blocking_reason
                .clone()
                .unwrap_or_else(|| "release security evidence is incomplete".to_owned()),
        ));
    }
    let metadata =
        std::fs::symlink_metadata(current_exe).map_err(|_| UpdateError::InvalidExecutable)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(UpdateError::InvalidExecutable);
    }
    let asset_name = candidate.asset_name.clone().ok_or_else(|| {
        UpdateError::SecurityBlocked("release target archive is missing".to_owned())
    })?;
    let expected_sha256 = candidate.security.sha256.clone().ok_or_else(|| {
        UpdateError::SecurityBlocked("release SHA-256 digest is missing".to_owned())
    })?;
    let expected_digest = format!("sha256:{expected_sha256}");
    let expected_version = candidate.version.clone();
    let expected_target = check.target.clone();
    let expected_output_version = format!("sigil {expected_version}");
    let expected_output_target = format!("target: {expected_target}");
    let expected_output_distribution = "distribution: github-release";

    let mut builder = self_update::backends::github::Update::configure();
    builder
        .repo_owner("JimmyDaddy")
        .repo_name("sigil")
        .current_version(&check.current_version)
        .release_tag(&candidate.tag_name)
        .target(&check.target)
        .bin_name("sigil")
        .bin_path_in_archive("sigil-{{ version }}-{{ target }}/{{ bin }}")
        .bin_install_path(current_exe)
        .asset_matcher(move |assets| {
            assets
                .iter()
                .find(|asset| {
                    asset.name() == asset_name && asset.digest() == Some(expected_digest.as_str())
                })
                .cloned()
        })
        .verify_checksum(self_update::Checksum::Sha256(expected_sha256))
        .verify_release_digest(true)
        .verify_binary(move |path| {
            let output = Command::new(path)
                .arg("--version")
                .output()
                .map_err(|error| self_update::Error::verification_rejected(error.to_string()))?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            if !output.status.success()
                || !stdout.lines().any(|line| line == expected_output_version)
                || !stdout.lines().any(|line| line == expected_output_target)
                || !stdout
                    .lines()
                    .any(|line| line == expected_output_distribution)
            {
                return Err(self_update::Error::verification_rejected(
                    "downloaded Sigil binary reported unexpected build metadata",
                ));
            }
            Ok(())
        })
        .unattended();
    builder.build_async()?.update_async().await?;
    Ok(UpdateApplyOutcome::Installed {
        version: expected_version,
    })
}

#[cfg(test)]
#[path = "tests/apply_tests.rs"]
mod tests;
