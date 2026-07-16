use std::{
    fs::{self, OpenOptions},
    io::Read,
    path::Path,
};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

pub(crate) const MAX_PLUGIN_MANIFEST_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BoundedPluginManifestReadError {
    Unavailable,
    TooLarge,
}

impl std::fmt::Display for BoundedPluginManifestReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Unavailable => "plugin manifest cannot be read as a regular file",
            Self::TooLarge => "plugin manifest exceeds the 1 MiB review size limit",
        })
    }
}

impl std::error::Error for BoundedPluginManifestReadError {}

pub(crate) fn read_bounded_plugin_manifest(
    path: &Path,
) -> std::result::Result<Vec<u8>, BoundedPluginManifestReadError> {
    let initial_metadata =
        fs::metadata(path).map_err(|_| BoundedPluginManifestReadError::Unavailable)?;
    if !initial_metadata.is_file() {
        return Err(BoundedPluginManifestReadError::Unavailable);
    }
    if initial_metadata.len() > MAX_PLUGIN_MANIFEST_BYTES as u64 {
        return Err(BoundedPluginManifestReadError::TooLarge);
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    let file = options
        .open(path)
        .map_err(|_| BoundedPluginManifestReadError::Unavailable)?;
    let metadata = file
        .metadata()
        .map_err(|_| BoundedPluginManifestReadError::Unavailable)?;
    if !metadata.is_file() {
        return Err(BoundedPluginManifestReadError::Unavailable);
    }
    if metadata.len() > MAX_PLUGIN_MANIFEST_BYTES as u64 {
        return Err(BoundedPluginManifestReadError::TooLarge);
    }
    #[cfg(unix)]
    if metadata.dev() != initial_metadata.dev() || metadata.ino() != initial_metadata.ino() {
        return Err(BoundedPluginManifestReadError::Unavailable);
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((MAX_PLUGIN_MANIFEST_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| BoundedPluginManifestReadError::Unavailable)?;
    if bytes.len() > MAX_PLUGIN_MANIFEST_BYTES {
        return Err(BoundedPluginManifestReadError::TooLarge);
    }
    Ok(bytes)
}

#[cfg(all(test, unix))]
#[path = "tests/plugin_manifest_io_tests.rs"]
mod tests;
