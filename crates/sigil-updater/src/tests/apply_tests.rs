use crate::{
    InstallSource, ReleaseSecurity, UpdateCandidate, UpdateChannel, UpdateCheckOutcome,
    UpdateError, apply_checked_update,
};

fn check(source: InstallSource, security: ReleaseSecurity) -> UpdateCheckOutcome {
    UpdateCheckOutcome {
        current_version: "1.0.0-beta.1".to_owned(),
        target: "aarch64-apple-darwin".to_owned(),
        channel: UpdateChannel::Beta,
        install_source: source,
        checked_at_unix_seconds: 42,
        cached: false,
        candidate: Some(UpdateCandidate {
            version: "1.0.0-beta.2".to_owned(),
            tag_name: "v1.0.0-beta.2".to_owned(),
            prerelease: true,
            asset_name: Some("sigil-1.0.0-beta.2-aarch64-apple-darwin.tar.gz".to_owned()),
            security,
        }),
        managed_update_command: None,
    }
}

#[tokio::test]
async fn npm_apply_delegates_without_touching_the_binary() -> Result<(), Box<dyn std::error::Error>>
{
    let outcome = apply_checked_update(
        &check(
            InstallSource::Npm,
            ReleaseSecurity {
                immutable: true,
                sha256: Some("a".repeat(64)),
                eligible_for_apply: true,
                blocking_reason: None,
            },
        ),
        std::path::Path::new("/does/not/exist"),
    )
    .await?;
    assert_eq!(
        outcome,
        crate::UpdateApplyOutcome::ManagedExternally {
            command: "npm install -g @sigil-ai/sigil@beta".to_owned(),
        }
    );
    Ok(())
}

#[tokio::test]
async fn standalone_apply_fails_closed_before_download_when_release_is_mutable() {
    let result = apply_checked_update(
        &check(
            InstallSource::StandaloneGitHubArchive,
            ReleaseSecurity {
                immutable: false,
                sha256: Some("a".repeat(64)),
                eligible_for_apply: false,
                blocking_reason: Some("GitHub release is mutable".to_owned()),
            },
        ),
        std::path::Path::new("/does/not/exist"),
    )
    .await;
    assert!(matches!(result, Err(UpdateError::SecurityBlocked(_))));
}
