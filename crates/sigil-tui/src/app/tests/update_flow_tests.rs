use std::{path::Path, sync::mpsc::sync_channel};

use sigil_updater::{
    InstallSource, ReleaseSecurity, UpdateCandidate, UpdateChannel, UpdateCheckOutcome,
};

use super::{AppAction, AppState, TimelineRole, UpdateTaskResult};
use crate::app::tests::common::test_config;

#[test]
fn update_slash_command_maps_check_refresh_and_apply_actions() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    assert!(matches!(
        app.execute_update_slash_command(""),
        Some(AppAction::CheckForUpdate {
            force_refresh: false,
            channel: UpdateChannel::Current,
        })
    ));
    assert!(matches!(
        app.execute_update_slash_command("refresh"),
        Some(AppAction::CheckForUpdate {
            force_refresh: true,
            channel: UpdateChannel::Current,
        })
    ));
    assert!(matches!(
        app.execute_update_slash_command("apply"),
        Some(AppAction::ApplyUpdate {
            channel: UpdateChannel::Current,
        })
    ));
    assert!(matches!(
        app.execute_update_slash_command("check beta"),
        Some(AppAction::CheckForUpdate {
            force_refresh: false,
            channel: UpdateChannel::Beta,
        })
    ));
}

#[test]
fn update_slash_command_resolves_through_the_shared_catalog() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input = "/update check beta".to_owned();

    let action = app.submit_input()?;

    assert!(matches!(
        action,
        Some(AppAction::CheckForUpdate {
            force_refresh: false,
            channel: UpdateChannel::Beta,
        })
    ));
    Ok(())
}

#[test]
fn source_build_skips_automatic_update_checks() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_update_build_info(sigil_updater::BuildMetadata::source(
        "1.0.0",
        "aarch64-apple-darwin",
        "release",
    ));

    assert!(!app.maybe_start_automatic_update_check());
    assert!(!app.has_pending_update_task());
    assert!(!app.maybe_start_automatic_update_check());

    assert!(app.start_update_check(true, true, UpdateChannel::Current));
    assert!(!app.has_pending_update_task());
    assert_eq!(
        app.last_notice(),
        Some("updates are unavailable for source builds; rebuild with Cargo instead")
    );
}

#[test]
fn explicit_update_result_is_rendered_as_a_timeline_notice() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let (sender, receiver) = sync_channel(1);
    app.update_state.task_rx = Some(receiver);
    sender
        .send(UpdateTaskResult::Checked {
            explicit: true,
            result: Ok(UpdateCheckOutcome {
                current_version: "1.0.0".to_owned(),
                target: "aarch64-apple-darwin".to_owned(),
                channel: UpdateChannel::Stable,
                install_source: InstallSource::StandaloneGitHubArchive,
                checked_at_unix_seconds: 1,
                cached: false,
                candidate: None,
                managed_update_command: None,
            }),
        })
        .expect("send update result");

    assert!(app.poll_update_task());
    assert_eq!(
        app.last_notice(),
        Some("Sigil 1.0.0 is up to date on the stable channel")
    );
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Notice
            && entry
                .text
                .contains("Sigil 1.0.0 is up to date on the stable channel")
    }));
}

#[test]
fn package_manager_update_result_preserves_the_owner_command() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let (sender, receiver) = sync_channel(1);
    app.update_state.task_rx = Some(receiver);
    sender
        .send(UpdateTaskResult::Checked {
            explicit: false,
            result: Ok(UpdateCheckOutcome {
                current_version: "1.0.0-beta.1".to_owned(),
                target: "aarch64-apple-darwin".to_owned(),
                channel: UpdateChannel::Current,
                install_source: InstallSource::Npm,
                checked_at_unix_seconds: 1,
                cached: false,
                candidate: Some(UpdateCandidate {
                    version: "1.0.0-beta.2".to_owned(),
                    tag_name: "v1.0.0-beta.2".to_owned(),
                    prerelease: true,
                    asset_name: Some("sigil-1.0.0-beta.2-aarch64-apple-darwin.tar.gz".to_owned()),
                    security: ReleaseSecurity {
                        immutable: true,
                        sha256: Some("a".repeat(64)),
                        eligible_for_apply: true,
                        blocking_reason: None,
                    },
                }),
                managed_update_command: Some("npm install -g @sigil-ai/sigil@beta".to_owned()),
            }),
        })
        .expect("send update result");

    assert!(app.poll_update_task());
    assert_eq!(
        app.last_notice(),
        Some("Sigil 1.0.0-beta.2 is available · run npm install -g @sigil-ai/sigil@beta")
    );
}
