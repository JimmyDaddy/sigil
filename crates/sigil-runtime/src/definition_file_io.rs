use std::{
    fs::{self, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
#[cfg(target_os = "macos")]
use std::{
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const MAX_DEFINITION_FILE_BYTES: usize = 1024 * 1024;

// Darwin marks cloud-backed placeholders with SF_DATALESS; opening one may trigger a download.
#[cfg(any(target_os = "macos", test))]
const DARWIN_SF_DATALESS: u32 = 0x4000_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DefinitionFileReadError {
    Unavailable,
    CloudPlaceholder,
    TooLarge,
    InvalidUtf8,
}

impl std::fmt::Display for DefinitionFileReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Unavailable => "definition cannot be read as a regular local file",
            Self::CloudPlaceholder => {
                "definition is a cloud placeholder that is not available locally"
            }
            Self::TooLarge => "definition exceeds the 1 MiB review size limit",
            Self::InvalidUtf8 => "definition is not UTF-8",
        })
    }
}

impl std::error::Error for DefinitionFileReadError {}

/// Requests local materialization for small workspace definition files without letting a cloud
/// placeholder block the caller indefinitely.
///
/// On macOS, iCloud placeholders are queued as one bounded batch and given a short shared window to
/// materialize. Files that remain remote are still skipped by [`read_definition_file`]. Other
/// platforms need no preparation.
pub(crate) fn prepare_definition_roots(roots: &[PathBuf]) {
    #[cfg(target_os = "macos")]
    prepare_macos_definition_roots(roots);

    #[cfg(not(target_os = "macos"))]
    let _ = roots;
}

pub(crate) fn read_definition_file(path: &Path) -> Result<Vec<u8>, DefinitionFileReadError> {
    let initial_metadata =
        fs::symlink_metadata(path).map_err(|_| DefinitionFileReadError::Unavailable)?;
    if !initial_metadata.file_type().is_file() {
        return Err(DefinitionFileReadError::Unavailable);
    }
    if metadata_is_dataless(&initial_metadata) {
        return Err(DefinitionFileReadError::CloudPlaceholder);
    }
    if initial_metadata.len() > MAX_DEFINITION_FILE_BYTES as u64 {
        return Err(DefinitionFileReadError::TooLarge);
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    let file = options
        .open(path)
        .map_err(|_| DefinitionFileReadError::Unavailable)?;
    let metadata = file
        .metadata()
        .map_err(|_| DefinitionFileReadError::Unavailable)?;
    if !metadata.is_file() {
        return Err(DefinitionFileReadError::Unavailable);
    }
    if metadata_is_dataless(&metadata) {
        return Err(DefinitionFileReadError::CloudPlaceholder);
    }
    if metadata.len() > MAX_DEFINITION_FILE_BYTES as u64 {
        return Err(DefinitionFileReadError::TooLarge);
    }
    #[cfg(unix)]
    if metadata.dev() != initial_metadata.dev() || metadata.ino() != initial_metadata.ino() {
        return Err(DefinitionFileReadError::Unavailable);
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((MAX_DEFINITION_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| DefinitionFileReadError::Unavailable)?;
    if bytes.len() > MAX_DEFINITION_FILE_BYTES {
        return Err(DefinitionFileReadError::TooLarge);
    }
    Ok(bytes)
}

pub(crate) fn read_definition_text(path: &Path) -> Result<String, DefinitionFileReadError> {
    String::from_utf8(read_definition_file(path)?).map_err(|_| DefinitionFileReadError::InvalidUtf8)
}

#[cfg(target_os = "macos")]
fn prepare_macos_definition_roots(roots: &[PathBuf]) {
    let mut candidates = Vec::new();
    for root in roots {
        collect_definition_files(root, 0, &mut candidates);
    }
    let placeholders = candidates
        .into_iter()
        .filter(|path| {
            fs::symlink_metadata(path).is_ok_and(|metadata| {
                metadata.file_type().is_file() && metadata_is_dataless(&metadata)
            })
        })
        .collect::<Vec<_>>();
    if placeholders.is_empty() {
        return;
    }

    let mut children = placeholders
        .iter()
        .filter_map(|path| {
            Command::new("/usr/bin/brctl")
                .arg("download")
                .arg(path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .ok()
        })
        .collect::<Vec<_>>();
    let process_deadline = Instant::now() + Duration::from_millis(250);
    while Instant::now() < process_deadline
        && children
            .iter_mut()
            .any(|child| child.try_wait().ok().flatten().is_none())
    {
        thread::sleep(Duration::from_millis(15));
    }
    for child in &mut children {
        if child.try_wait().ok().flatten().is_none() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    let deadline = Instant::now() + Duration::from_millis(300);
    while Instant::now() < deadline
        && placeholders.iter().any(|path| {
            fs::symlink_metadata(path).is_ok_and(|metadata| metadata_is_dataless(&metadata))
        })
    {
        thread::sleep(Duration::from_millis(15));
    }
}

#[cfg(target_os = "macos")]
fn collect_definition_files(root: &Path, depth: usize, files: &mut Vec<PathBuf>) {
    if depth > 2 {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_file() {
            files.push(path);
        } else if file_type.is_dir() && !file_type.is_symlink() {
            collect_definition_files(&path, depth + 1, files);
        }
    }
}

#[cfg(target_os = "macos")]
fn metadata_is_dataless(metadata: &fs::Metadata) -> bool {
    use std::os::macos::fs::MetadataExt;

    darwin_file_flags_are_dataless(metadata.st_flags())
}

#[cfg(not(target_os = "macos"))]
fn metadata_is_dataless(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(any(target_os = "macos", test))]
fn darwin_file_flags_are_dataless(flags: u32) -> bool {
    flags & DARWIN_SF_DATALESS != 0
}

#[cfg(test)]
#[path = "tests/definition_file_io_tests.rs"]
mod tests;
