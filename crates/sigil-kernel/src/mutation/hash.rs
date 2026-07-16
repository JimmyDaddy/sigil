use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};

pub fn file_content_hash(path: &Path) -> Result<Option<String>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes_hash(&bytes))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

pub(super) fn directory_state_hash(path: &Path) -> Result<Option<String>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            Ok(Some(directory_present_hash()))
        }
        Ok(_) => bail!("path is not a directory: {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

pub(super) fn compare_current_directory_hash(path: &Path, expected: Option<&str>) -> Result<()> {
    let current = directory_state_hash(path)?;
    if current.as_deref() != expected {
        bail!(
            "directory changed before controlled mutation commit: {}",
            path.display()
        );
    }
    Ok(())
}

pub(super) fn ensure_empty_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        bail!("path is not a directory: {}", path.display());
    }
    let mut entries = fs::read_dir(path)
        .with_context(|| format!("failed to read directory {}", path.display()))?;
    if entries.next().transpose()?.is_some() {
        bail!(
            "non-empty directory delete is not supported by controlled mutation protocol: {}",
            path.display()
        );
    }
    Ok(())
}

pub(super) fn directory_present_hash() -> String {
    bytes_hash(b"sigil:directory:present:v1")
}

pub fn bytes_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("sha256:{digest:x}")
}

pub(super) fn compare_current_hash(path: &Path, expected: Option<&str>) -> Result<()> {
    let current = file_content_hash(path)?;
    if current.as_deref() != expected {
        bail!(
            "file changed before controlled mutation commit: {}",
            path.display()
        );
    }
    Ok(())
}

pub(super) fn atomic_replace(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("target path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let existing_permissions = fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions());
    let temp_path = temp_path_for(path);
    {
        let mut temp_file = File::create(&temp_path)
            .with_context(|| format!("failed to create {}", temp_path.display()))?;
        temp_file
            .write_all(content)
            .with_context(|| format!("failed to write {}", temp_path.display()))?;
        if let Some(permissions) = existing_permissions {
            fs::set_permissions(&temp_path, permissions).with_context(|| {
                format!("failed to preserve permissions for {}", path.display())
            })?;
        }
        temp_file
            .sync_all()
            .with_context(|| format!("failed to sync {}", temp_path.display()))?;
    }
    fs::rename(&temp_path, path).with_context(|| atomic_replace_error_message(path, &temp_path))?;
    sync_published_file(path)?;
    sync_parent(path)
}

pub(super) fn ensure_observed_after_hash_matches_intent(
    observed_after_hash: &Option<String>,
    intended_hash: &str,
) -> Result<()> {
    if observed_after_hash.as_deref() != Some(intended_hash) {
        bail!("observed file hash does not match intended hash after write");
    }
    Ok(())
}

pub(super) fn atomic_replace_error_message(path: &Path, temp_path: &Path) -> String {
    format!(
        "failed to atomically replace {} with {}",
        path.display(),
        temp_path.display()
    )
}

#[cfg(unix)]
fn sync_published_file(path: &Path) -> Result<()> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync {}", path.display()))
}

#[cfg(not(unix))]
fn sync_published_file(_path: &Path) -> Result<()> {
    // The temporary file is synced before publication. Rust cannot portably reopen a published
    // read-only file with the write access Windows requires for FlushFileBuffers.
    Ok(())
}

#[cfg(unix)]
pub(super) fn sync_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("target path has no parent: {}", path.display()))?;
    let parent_file =
        File::open(parent).with_context(|| format!("failed to open {}", parent.display()))?;
    parent_file
        .sync_all()
        .with_context(|| format!("failed to sync {}", parent.display()))
}

#[cfg(not(unix))]
pub(super) fn sync_parent(_path: &Path) -> Result<()> {
    // Rust's standard library cannot portably open and flush directory handles on Windows. File
    // contents are synced before this boundary; directory-entry flushing is a platform limit.
    Ok(())
}

pub(super) fn temp_path_for(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("mutation");
    let temp_name = format!(".{file_name}.sigil-tmp-{}", std::process::id());
    path.with_file_name(temp_name)
}

pub(super) fn short_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!("{digest:x}").chars().take(16).collect()
}

pub(super) fn unix_time_ms() -> u64 {
    system_time_to_unix_ms(SystemTime::now()).unwrap_or(0)
}

pub(super) fn file_modified_ms(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(system_time_to_unix_ms)
}

pub(super) fn system_time_to_unix_ms(time: SystemTime) -> Option<u64> {
    let duration = time.duration_since(UNIX_EPOCH).ok()?;
    Some(
        duration
            .as_secs()
            .saturating_mul(1_000)
            .saturating_add(u64::from(duration.subsec_millis())),
    )
}

pub(super) fn artifact_blob_matches(path: &Path, expected_hash: &str) -> Result<bool> {
    match fs::read(path) {
        Ok(bytes) => Ok(bytes_hash(&bytes) == expected_hash),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

pub(super) fn atomic_write_artifact(path: &Path, bytes: &[u8]) -> Result<()> {
    let temp_path = temp_path_for(path);
    {
        let mut temp_file = File::create(&temp_path)
            .with_context(|| format!("failed to create {}", temp_path.display()))?;
        temp_file
            .write_all(bytes)
            .with_context(|| format!("failed to write {}", temp_path.display()))?;
        temp_file
            .sync_all()
            .with_context(|| format!("failed to sync {}", temp_path.display()))?;
    }
    fs::rename(&temp_path, path).with_context(|| {
        format!(
            "failed to atomically replace artifact {} with {}",
            path.display(),
            temp_path.display()
        )
    })?;
    sync_published_file(path)?;
    sync_parent(path)
}

pub(super) fn remove_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

#[cfg(unix)]
pub(super) fn sync_existing_dir(path: &Path) -> Result<()> {
    match File::open(path) {
        Ok(file) => file
            .sync_all()
            .with_context(|| format!("failed to sync {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to open {}", path.display())),
    }
}

#[cfg(not(unix))]
pub(super) fn sync_existing_dir(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
pub(super) fn harden_artifact_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to set permissions on {}", path.display()))
}

#[cfg(not(unix))]
pub(super) fn harden_artifact_dir(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
pub(super) fn harden_artifact_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to set permissions on {}", path.display()))
}

#[cfg(not(unix))]
pub(super) fn harden_artifact_file(_path: &Path) -> Result<()> {
    Ok(())
}
