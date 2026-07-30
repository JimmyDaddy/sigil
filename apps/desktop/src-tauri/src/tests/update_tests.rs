use tauri_plugin_updater::Error as UpdaterError;

use super::{
    BACKGROUND_CHECK_INTERVAL, DesktopUpdateCommandError, DesktopUpdateLastCheck,
    DesktopUpdatePhase, DesktopUpdateSession, background_check_due_at, bounded_chars,
    project_updater_error, runtime_updates_supported, validate_update_asset, write_last_check,
};

#[test]
fn debug_builds_fail_closed_as_unsupported() {
    assert!(!runtime_updates_supported());
    let mut session = DesktopUpdateSession::new("0.0.1-beta.1".to_owned(), false);

    let error = session
        .begin_check()
        .expect_err("debug update check should be rejected");

    assert_eq!(session.snapshot.phase, DesktopUpdatePhase::Unsupported);
    assert_eq!(error.code, "update_not_supported");
}

#[test]
fn update_state_transitions_clear_stale_progress_and_errors() {
    let mut session = DesktopUpdateSession::new("0.0.1-beta.1".to_owned(), true);
    session.snapshot.downloaded_bytes = 42;
    session.snapshot.total_bytes = Some(84);
    session.snapshot.error_code = Some("update_check_failed");

    session
        .begin_check()
        .expect("release update check should start");
    assert_eq!(session.snapshot.phase, DesktopUpdatePhase::Checking);
    assert_eq!(session.snapshot.downloaded_bytes, 0);
    assert_eq!(session.snapshot.total_bytes, None);
    assert_eq!(session.snapshot.error_code, None);

    session.set_up_to_date();
    assert_eq!(session.snapshot.phase, DesktopUpdatePhase::UpToDate);
    session.record_progress(16, Some(64));
    session.record_progress(12, Some(64));
    assert_eq!(session.snapshot.downloaded_bytes, 28);
    assert_eq!(session.snapshot.total_bytes, Some(64));
}

#[test]
fn restart_failures_preserve_the_installed_ready_state() {
    let mut session = DesktopUpdateSession::new("0.0.1-beta.1".to_owned(), true);
    session.set_ready_to_restart();

    session.set_restart_error("update_restart_cleanup_failed");

    assert_eq!(session.snapshot.phase, DesktopUpdatePhase::ReadyToRestart);
    assert_eq!(
        session.snapshot.error_code,
        Some("update_restart_cleanup_failed")
    );
}

#[test]
fn updater_assets_require_the_immutable_github_https_route_and_signature() {
    assert!(
        validate_update_asset(
            "https",
            Some("github.com"),
            "/JimmyDaddy/sigil/releases/download/v0.0.1-beta.2/Sigil.app.tar.gz",
            "signed payload"
        )
        .is_ok()
    );
    assert_eq!(
        validate_update_asset(
            "http",
            Some("github.com"),
            "/JimmyDaddy/sigil/releases/download/v0.0.1-beta.2/Sigil.app.tar.gz",
            "signed payload"
        )
        .expect_err("insecure asset should fail")
        .code,
        "update_manifest_invalid"
    );
    assert_eq!(
        validate_update_asset(
            "https",
            Some("example.com"),
            "/JimmyDaddy/sigil/releases/download/v0.0.1-beta.2/Sigil.app.tar.gz",
            "signed payload"
        )
        .expect_err("unapproved asset host should fail")
        .code,
        "update_manifest_invalid"
    );
    assert_eq!(
        validate_update_asset(
            "https",
            Some("github.com"),
            "/JimmyDaddy/sigil/releases/download/v0.0.1-beta.2/Sigil.app.tar.gz",
            " "
        )
        .expect_err("missing signature should fail")
        .code,
        "update_manifest_invalid"
    );
    assert_eq!(
        validate_update_asset(
            "https",
            Some("github.com"),
            "/JimmyDaddy/sigil/releases/latest/download/Sigil.app.tar.gz",
            "signed payload"
        )
        .expect_err("mutable latest assets should fail")
        .code,
        "update_manifest_invalid"
    );
    assert_eq!(
        validate_update_asset(
            "https",
            Some("github.com"),
            "/JimmyDaddy/sigil/releases/download/v0.0.1-beta.2/Sigil.dmg",
            "signed payload"
        )
        .expect_err("desktop updater should require app archives")
        .code,
        "update_manifest_invalid"
    );
}

#[test]
fn updater_failures_are_projected_without_raw_transport_details() {
    assert_eq!(
        project_updater_error(&UpdaterError::ReleaseNotFound).code,
        "update_manifest_invalid"
    );
    assert_eq!(
        project_updater_error(&UpdaterError::Network(
            "server included a private diagnostic".to_owned()
        ))
        .code,
        "update_check_failed"
    );
}

#[test]
fn release_notes_are_bounded_by_unicode_scalar_count() {
    let bounded = bounded_chars("你".repeat(5).as_str(), 3);
    assert_eq!(bounded, "你你你");
}

#[test]
fn command_errors_serialize_as_bounded_structured_failures() {
    let value = serde_json::to_value(DesktopUpdateCommandError::new(
        "update_busy",
        "Another update is running.",
    ))
    .expect("update error should serialize");

    assert_eq!(value["code"], "update_busy");
    assert_eq!(value["message"], "Another update is running.");
}

#[test]
fn background_checks_run_at_most_once_per_day() {
    let last = 10_000;
    assert!(!background_check_due_at(
        last,
        last + BACKGROUND_CHECK_INTERVAL.as_secs() - 1
    ));
    assert!(background_check_due_at(
        last,
        last + BACKGROUND_CHECK_INTERVAL.as_secs()
    ));
    assert!(!background_check_due_at(last + 1_000, last));
}

#[test]
fn successful_check_timestamp_is_private_and_parseable() {
    let temp = tempfile::tempdir().expect("temporary directory should create");
    let path = temp.path().join("updates").join("last-check.json");
    write_last_check(&path, 42).expect("last check should persist");
    let parsed: DesktopUpdateLastCheck =
        serde_json::from_slice(&std::fs::read(&path).expect("last check should read"))
            .expect("last check should decode");

    assert_eq!(parsed.checked_at_unix_seconds, 42);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path)
                .expect("last check metadata should read")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
