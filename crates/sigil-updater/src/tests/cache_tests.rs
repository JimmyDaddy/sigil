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

#[tokio::test]
async fn product_updater_owner_publishes_one_closed_atomic_receipt() {
    let directory = tempfile::tempdir().expect("tempdir");
    let owner = super::ProductUpdaterState::from_cache_root(directory.path());
    let entry = UpdateCacheEntry::new("owner-key".to_owned(), 9, None, outcome());

    let receipt = owner
        .replace(&entry)
        .await
        .expect("owner publish should succeed");
    assert_ne!(receipt.object_hash(), [0; 32]);
    assert_eq!(owner.load().await.expect("published entry"), entry);
    let error = owner
        .replace(&entry)
        .await
        .expect_err("duplicate owner replace must fail closed");
    assert!(error.to_string().contains("already current"));
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
