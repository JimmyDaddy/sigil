use semver::Version;

use super::{GitHubAsset, GitHubRelease, select_candidate};
use crate::UpdateChannel;

fn release(
    tag_name: &str,
    prerelease: bool,
    immutable: bool,
    assets: Vec<GitHubAsset>,
) -> GitHubRelease {
    GitHubRelease {
        tag_name: tag_name.to_owned(),
        draft: false,
        prerelease,
        immutable,
        assets,
    }
}

fn asset(name: &str, digest: Option<&str>) -> GitHubAsset {
    GitHubAsset {
        name: name.to_owned(),
        digest: digest.map(str::to_owned),
    }
}

#[tokio::test]
async fn successful_check_retains_candidate_when_cache_publication_fails() {
    let directory = tempfile::tempdir().expect("cache root");
    std::fs::write(directory.path().join("updates"), b"not a directory")
        .expect("block only the advisory cache path");
    let service = super::UpdateService::github(directory.path()).expect("checker");
    let digest = format!("sha256:{}", "a".repeat(64));
    let current = Version::parse("1.0.0").expect("current version");
    let candidate = select_candidate(
        &current,
        "aarch64-apple-darwin",
        UpdateChannel::Stable,
        &[release(
            "v1.1.0",
            false,
            true,
            vec![asset(
                "sigil-1.1.0-aarch64-apple-darwin.tar.gz",
                Some(&digest),
            )],
        )],
    );
    for cached in [false, true] {
        let outcome = super::UpdateCheckOutcome {
            current_version: "1.0.0".to_owned(),
            target: "aarch64-apple-darwin".to_owned(),
            channel: UpdateChannel::Stable,
            install_source: crate::InstallSource::StandaloneGitHubArchive,
            checked_at_unix_seconds: 42,
            cached,
            candidate: candidate.clone(),
            managed_update_command: None,
        };
        let persisted = super::UpdateCacheEntry::new(
            "key".to_owned(),
            42,
            Some("etag".to_owned()),
            outcome.clone(),
        );
        let retained = service.finish_check(outcome.clone(), persisted).await;
        assert_eq!(retained, outcome);
        assert!(retained.update_available());
        assert!(retained.apply_permitted());
        assert!(service.product_state.load().await.is_none());
    }
}

#[test]
fn beta_selects_highest_semver_and_can_promote_to_stable() -> Result<(), Box<dyn std::error::Error>>
{
    let current = Version::parse("1.0.0-beta.1")?;
    let digest = format!("sha256:{}", "a".repeat(64));
    let releases = vec![
        release(
            "v1.0.0-beta.2",
            true,
            true,
            vec![asset(
                "sigil-1.0.0-beta.2-aarch64-apple-darwin.tar.gz",
                Some(&digest),
            )],
        ),
        release(
            "v1.0.0",
            false,
            true,
            vec![asset(
                "sigil-1.0.0-aarch64-apple-darwin.tar.gz",
                Some(&digest),
            )],
        ),
    ];

    let selected = select_candidate(
        &current,
        "aarch64-apple-darwin",
        UpdateChannel::Beta,
        &releases,
    )
    .ok_or("expected a selected release")?;
    assert_eq!(selected.version, "1.0.0");
    assert!(selected.security.eligible_for_apply);
    assert_eq!(selected.security.sha256, Some("a".repeat(64)));
    Ok(())
}

#[test]
fn beta_release_selection_rejects_alpha_and_rc_tags() -> Result<(), Box<dyn std::error::Error>> {
    let current = Version::parse("1.0.0-beta.1")?;
    let releases = vec![
        release("v1.0.0-rc.1", true, true, Vec::new()),
        release("v1.0.0-beta.2", true, true, Vec::new()),
        release("v1.0.0-alpha.9", true, true, Vec::new()),
    ];

    let selected = select_candidate(
        &current,
        "aarch64-apple-darwin",
        UpdateChannel::Beta,
        &releases,
    )
    .ok_or("expected beta selection")?;

    assert_eq!(selected.version, "1.0.0-beta.2");
    Ok(())
}

#[test]
fn current_release_selection_keeps_the_installed_alpha_channel()
-> Result<(), Box<dyn std::error::Error>> {
    let current = Version::parse("1.0.0-alpha.1")?;
    let releases = vec![
        release("v1.0.0-rc.1", true, true, Vec::new()),
        release("v1.0.0-beta.1", true, true, Vec::new()),
        release("v1.0.0-alpha.2", true, true, Vec::new()),
    ];

    let selected = select_candidate(
        &current,
        "aarch64-apple-darwin",
        UpdateChannel::Current,
        &releases,
    )
    .ok_or("expected alpha selection")?;

    assert_eq!(selected.version, "1.0.0-alpha.2");
    Ok(())
}

#[test]
fn stable_skips_github_prerelease_even_when_it_has_a_higher_version()
-> Result<(), Box<dyn std::error::Error>> {
    let current = Version::parse("1.0.0")?;
    let releases = vec![
        release("v1.1.0-beta.1", true, true, Vec::new()),
        release("v1.0.1", false, true, Vec::new()),
    ];

    let selected = select_candidate(
        &current,
        "x86_64-unknown-linux-gnu",
        UpdateChannel::Stable,
        &releases,
    )
    .ok_or("expected stable selection")?;
    assert_eq!(selected.version, "1.0.1");
    Ok(())
}

#[test]
fn mutable_or_digestless_release_can_be_reported_but_not_applied()
-> Result<(), Box<dyn std::error::Error>> {
    let current = Version::parse("1.0.0")?;
    let releases = vec![release(
        "v1.0.1",
        false,
        false,
        vec![asset("sigil-1.0.1-x86_64-pc-windows-msvc.tar.gz", None)],
    )];

    let selected = select_candidate(
        &current,
        "x86_64-pc-windows-msvc",
        UpdateChannel::Stable,
        &releases,
    )
    .ok_or("expected mutable release to remain visible")?;
    assert!(!selected.security.eligible_for_apply);
    assert_eq!(
        selected.security.blocking_reason.as_deref(),
        Some("GitHub release is mutable")
    );
    Ok(())
}

#[test]
fn exact_asset_name_does_not_accept_checksum_or_npm_assets()
-> Result<(), Box<dyn std::error::Error>> {
    let current = Version::parse("1.0.0")?;
    let digest = format!("sha256:{}", "b".repeat(64));
    let releases = vec![release(
        "v1.0.1",
        false,
        true,
        vec![
            asset(
                "sigil-1.0.1-x86_64-unknown-linux-gnu.tar.gz.sha256",
                Some(&digest),
            ),
            asset("sigil-ai-1.0.1.tgz", Some(&digest)),
        ],
    )];

    let selected = select_candidate(
        &current,
        "x86_64-unknown-linux-gnu",
        UpdateChannel::Stable,
        &releases,
    )
    .ok_or("expected release visibility")?;
    assert!(selected.asset_name.is_none());
    assert!(!selected.security.eligible_for_apply);
    Ok(())
}
