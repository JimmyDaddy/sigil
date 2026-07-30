use std::fs;

use super::{UpdateCacheEntry, load_sync, store_sync};
use crate::{InstallSource, UpdateChannel, UpdateCheckOutcome};

fn outcome() -> UpdateCheckOutcome {
    UpdateCheckOutcome {
        current_version: "1.0.0".to_owned(),
        target: "aarch64-apple-darwin".to_owned(),
        channel: UpdateChannel::Stable,
        install_source: InstallSource::StandaloneGitHubArchive,
        checked_at_unix_seconds: 42,
        cached: false,
        candidate: None,
        managed_update_command: None,
    }
}

#[test]
fn cache_round_trip_is_bounded_and_schema_checked() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("updates/v1/check.json");
    let entry = UpdateCacheEntry::new("key".to_owned(), 42, Some("\"etag\"".to_owned()), outcome());

    store_sync(&path, &entry)?;
    let loaded = load_sync(&path).ok_or("expected cache entry")?;
    assert_eq!(loaded.cache_key, "key");
    assert_eq!(loaded.etag.as_deref(), Some("\"etag\""));
    assert!(!loaded.outcome.cached);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(fs::metadata(path)?.permissions().mode() & 0o777, 0o600);
    }
    Ok(())
}

#[test]
fn symlink_cache_is_not_trusted() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let target = directory.path().join("target.json");
    fs::write(&target, b"{}")?;
    let link = directory.path().join("check.json");
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, &link)?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(target, &link)?;

    assert!(load_sync(&link).is_none());
    Ok(())
}
