use std::path::Path;

use super::{
    AutomaticCheckEnvironment, InstallSource, automatic_check_allowed, managed_update_command,
    resolve_install_source,
};
use crate::UpdateChannel;

#[test]
fn explicit_npm_marker_wins_over_archive_distribution() {
    let source = resolve_install_source(
        "github-release",
        Path::new("/opt/sigil/bin/sigil"),
        Some("npm"),
    );
    assert_eq!(source, InstallSource::Npm);
}

#[test]
fn environment_marker_cannot_promote_a_source_build_to_standalone() {
    for marker in ["github-release", "standalone"] {
        assert_eq!(
            resolve_install_source("source", Path::new("/opt/sigil/bin/sigil"), Some(marker),),
            InstallSource::Source
        );
    }
}

#[test]
fn only_the_github_release_marker_grants_binary_replacement_authority() {
    let path = Path::new("/opt/sigil/bin/sigil");
    let standalone = resolve_install_source("github-release", path, None);
    assert_eq!(standalone, InstallSource::StandaloneGitHubArchive);
    assert!(standalone.permits_binary_replacement());

    for distribution in ["source", "development", "", "unrecognized"] {
        let source = resolve_install_source(distribution, path, None);
        assert!(!source.permits_binary_replacement());
    }
}

#[test]
fn homebrew_and_cargo_paths_are_classified_conservatively() {
    assert_eq!(
        resolve_install_source(
            "github-release",
            Path::new("/opt/homebrew/Cellar/sigil-ai/0.1.0/bin/sigil"),
            None,
        ),
        InstallSource::Homebrew
    );
    assert_eq!(
        resolve_install_source("source", Path::new("/Users/alice/.cargo/bin/sigil"), None,),
        InstallSource::Cargo
    );
}

#[test]
fn direct_npm_platform_binary_path_keeps_package_manager_ownership() {
    assert_eq!(
        resolve_install_source(
            "github-release",
            Path::new("/usr/local/lib/node_modules/@sigil-ai/sigil-darwin-arm64/bin/sigil"),
            None,
        ),
        InstallSource::Npm
    );
}

#[test]
fn automatic_checks_require_packaged_release_builds() {
    let normal = AutomaticCheckEnvironment {
        ci: false,
        disabled: false,
    };
    assert!(automatic_check_allowed(
        "release",
        InstallSource::StandaloneGitHubArchive,
        normal,
    ));
    assert!(!automatic_check_allowed(
        "debug",
        InstallSource::StandaloneGitHubArchive,
        normal,
    ));
    assert!(!automatic_check_allowed(
        "release",
        InstallSource::Source,
        normal,
    ));
    assert!(!automatic_check_allowed(
        "release",
        InstallSource::Npm,
        AutomaticCheckEnvironment {
            ci: true,
            disabled: false,
        },
    ));
}

#[test]
fn package_manager_commands_preserve_original_owner() {
    assert_eq!(
        managed_update_command(InstallSource::Npm, UpdateChannel::Beta, "1.0.0-beta.2"),
        Some("npm install -g @sigil-ai/sigil@beta".to_owned())
    );
    assert_eq!(
        managed_update_command(InstallSource::Npm, UpdateChannel::Stable, "1.0.0"),
        Some("npm install -g @sigil-ai/sigil@latest".to_owned())
    );
    assert_eq!(
        managed_update_command(InstallSource::Npm, UpdateChannel::Beta, "1.0.0"),
        Some("npm install -g @sigil-ai/sigil@latest".to_owned())
    );
    assert_eq!(
        managed_update_command(InstallSource::Npm, UpdateChannel::Current, "1.0.0-alpha.7"),
        Some("npm install -g @sigil-ai/sigil@1.0.0-alpha.7".to_owned())
    );
    assert_eq!(
        managed_update_command(InstallSource::Homebrew, UpdateChannel::Stable, "1.0.0"),
        Some("brew upgrade sigil-ai".to_owned())
    );
}
