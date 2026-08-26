//! Owner-only filesystem primitives shared by durable authority snapshots.
//!
//! Domain journals retain their own schemas and append preconditions. This module owns only the
//! host-filesystem exclusion primitive so every full-snapshot writer gets the same no-follow,
//! owner-only and cross-process behavior.

use std::fs::{self, File, OpenOptions};
use std::path::Path;

use fs2::FileExt;

pub(crate) fn open_owner_only_snapshot_writer_lock(snapshot: &Path) -> std::io::Result<File> {
    let file_name = snapshot
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "snapshot path has no file name",
            )
        })?;
    let lock_path = snapshot.with_file_name(format!("{file_name}.lock"));
    if let Ok(metadata) = fs::symlink_metadata(&lock_path)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "snapshot writer lock is not a regular file",
        ));
    }

    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(&lock_path)?;
    let metadata = fs::symlink_metadata(&lock_path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "snapshot writer lock changed identity",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600))?;
    }
    file.try_lock_exclusive().map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!("snapshot writer lock is unavailable: {error}"),
        )
    })?;
    Ok(file)
}
