use std::{
    fs::{self, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sigil_kernel::{atomic_publish_private_file, secure_private_path_permissions};

use super::ModelCatalogEntry;

pub(super) const CATALOG_FRESH_TTL_SECS: u64 = 10 * 60;
pub(super) const CATALOG_STALE_WINDOW_SECS: u64 = 7 * 24 * 60 * 60;
const CATALOG_RETENTION_SECS: u64 = 30 * 24 * 60 * 60;
const CATALOG_FUTURE_CLOCK_SKEW_SECS: u64 = 5 * 60;
const CATALOG_CACHE_MAX_BYTES: usize = 1024 * 1024;
const CATALOG_SWEEP_MAX_ENTRIES: usize = 8_192;
const CATALOG_ATOMIC_TEMP_GRACE_SECS: u64 = 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogCacheWire {
    version: u32,
    connection_id: String,
    fingerprint: String,
    stored_at_unix_secs: u64,
    entries: Vec<ModelCatalogEntry>,
}

#[derive(Debug, Clone)]
pub(super) struct CachedCatalog {
    pub stored_at_unix_secs: u64,
    pub entries: Vec<ModelCatalogEntry>,
}

pub(super) fn load_catalog_cache(
    cache_root: &Path,
    connection_id: &str,
    fingerprint: &str,
) -> Option<CachedCatalog> {
    validate_catalog_tree(cache_root, connection_id).ok()?;
    let path = catalog_cache_path(cache_root, connection_id, fingerprint);
    let metadata = fs::symlink_metadata(&path).ok()?;
    if !catalog_file_metadata_is_safe(&metadata) || metadata.len() > CATALOG_CACHE_MAX_BYTES as u64
    {
        let _ = remove_catalog_cache_file(cache_root, connection_id, fingerprint);
        return None;
    }
    #[cfg(windows)]
    secure_private_path_permissions(&path).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            let _ = remove_catalog_cache_file(cache_root, connection_id, fingerprint);
            return None;
        }
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let file = options.open(&path).ok()?;
    let opened_metadata = file.metadata().ok()?;
    if !catalog_file_metadata_is_safe(&opened_metadata)
        || opened_metadata.len() > CATALOG_CACHE_MAX_BYTES as u64
    {
        return None;
    }
    #[cfg(unix)]
    if metadata.dev() != opened_metadata.dev() || metadata.ino() != opened_metadata.ino() {
        return None;
    }
    let mut bytes = Vec::with_capacity(opened_metadata.len() as usize);
    file.take((CATALOG_CACHE_MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > CATALOG_CACHE_MAX_BYTES {
        return None;
    }
    let wire: CatalogCacheWire = match serde_json::from_slice(&bytes) {
        Ok(wire) => wire,
        Err(_) => {
            let _ = remove_catalog_cache_file(cache_root, connection_id, fingerprint);
            return None;
        }
    };
    if wire.version != 2
        || wire.connection_id != connection_id
        || wire.fingerprint != fingerprint
        || wire.entries.len() > 2_000
        || wire.entries.iter().any(|entry| {
            entry.model_ref.connection_id.as_str() != connection_id
                || entry.provenance != super::ModelCatalogProvenance::Remote
                || entry.availability == super::ModelAvailability::ConfiguredUnavailable
        })
    {
        let _ = remove_catalog_cache_file(cache_root, connection_id, fingerprint);
        return None;
    }
    let now = now_unix_secs();
    if wire.stored_at_unix_secs > now.saturating_add(CATALOG_FUTURE_CLOCK_SKEW_SECS)
        || now.saturating_sub(wire.stored_at_unix_secs) > CATALOG_RETENTION_SECS
    {
        let _ = remove_catalog_cache_file(cache_root, connection_id, fingerprint);
        return None;
    }
    let mut entries = wire.entries;
    for entry in &mut entries {
        entry.recommendation = super::ModelRecommendation::Standard;
    }
    Some(CachedCatalog {
        stored_at_unix_secs: wire.stored_at_unix_secs,
        entries,
    })
}

pub(super) fn save_catalog_cache(
    cache_root: &Path,
    connection_id: &str,
    fingerprint: &str,
    entries: &[ModelCatalogEntry],
) -> Result<()> {
    anyhow::ensure!(entries.len() <= 2_000, "catalog cache entry limit exceeded");
    anyhow::ensure!(
        entries.iter().all(|entry| {
            entry.model_ref.connection_id.as_str() == connection_id
                && entry.provenance == super::ModelCatalogProvenance::Remote
                && entry.availability != super::ModelAvailability::ConfiguredUnavailable
        }),
        "catalog cache entries do not match the exact remote connection"
    );
    let path = catalog_cache_path(cache_root, connection_id, fingerprint);
    secure_catalog_tree(cache_root, connection_id)?;
    let bytes = serde_json::to_vec(&CatalogCacheWire {
        version: 2,
        connection_id: connection_id.to_owned(),
        fingerprint: fingerprint.to_owned(),
        stored_at_unix_secs: now_unix_secs(),
        entries: entries.to_vec(),
    })
    .context("failed to encode catalog cache")?;
    anyhow::ensure!(
        bytes.len() <= CATALOG_CACHE_MAX_BYTES,
        "catalog cache serialization limit exceeded"
    );
    atomic_publish_private_file(&path, &bytes)
}

pub(super) fn sweep_catalog_cache(cache_root: &Path) -> Result<()> {
    let root = cache_root.join("provider-models").join("v1");
    validate_catalog_path_ancestors(&root)?;
    match fs::symlink_metadata(&root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        Ok(_) => {
            validate_catalog_directory(&root, true)?;
        }
    }
    let mut visited = 0usize;
    for connection in fs::read_dir(&root)
        .with_context(|| format!("failed to read catalog cache root {}", root.display()))?
    {
        visited += 1;
        anyhow::ensure!(
            visited <= CATALOG_SWEEP_MAX_ENTRIES,
            "catalog cache sweep entry limit exceeded"
        );
        let connection = connection?;
        let connection_path = connection.path();
        validate_catalog_directory(&connection_path, true)?;
        let Some(connection_id) = connection.file_name().to_str().map(str::to_owned) else {
            anyhow::bail!("catalog cache connection directory is not UTF-8");
        };
        for entry in fs::read_dir(&connection_path).with_context(|| {
            format!(
                "failed to read catalog connection cache {}",
                connection_path.display()
            )
        })? {
            visited += 1;
            anyhow::ensure!(
                visited <= CATALOG_SWEEP_MAX_ENTRIES,
                "catalog cache sweep entry limit exceeded"
            );
            let entry = entry?;
            let path = entry.path();
            let file_name = entry.file_name();
            let metadata = fs::symlink_metadata(&path)?;
            if !catalog_file_metadata_is_safe(&metadata)
                || metadata.len() > CATALOG_CACHE_MAX_BYTES as u64
            {
                let _ =
                    remove_catalog_cache_entry(cache_root, &connection_id, file_name.as_os_str());
                continue;
            }
            if catalog_atomic_temp_is_recent(file_name.as_os_str(), &metadata) {
                continue;
            }
            let Some(fingerprint) = file_name
                .to_str()
                .and_then(|name| name.strip_suffix(".json"))
            else {
                let _ =
                    remove_catalog_cache_entry(cache_root, &connection_id, file_name.as_os_str());
                continue;
            };
            if load_catalog_cache(cache_root, &connection_id, fingerprint).is_none() {
                let _ = remove_catalog_cache_file(cache_root, &connection_id, fingerprint);
            }
        }
    }
    Ok(())
}

fn catalog_atomic_temp_is_recent(file_name: &std::ffi::OsStr, metadata: &fs::Metadata) -> bool {
    let Some(name) = file_name.to_str() else {
        return false;
    };
    let Some(body) = name
        .strip_prefix('.')
        .and_then(|name| name.strip_suffix(".tmp"))
    else {
        return false;
    };
    let Some((target, nonce)) = body.rsplit_once('.') else {
        return false;
    };
    if !target.ends_with(".json")
        || nonce.len() != 32
        || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return false;
    }
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    let now = SystemTime::now();
    match now.duration_since(modified) {
        Ok(age) => age.as_secs() <= CATALOG_ATOMIC_TEMP_GRACE_SECS,
        Err(error) => error.duration().as_secs() <= CATALOG_FUTURE_CLOCK_SKEW_SECS,
    }
}

pub(super) fn cache_age_secs(cache: &CachedCatalog) -> u64 {
    let now = now_unix_secs();
    if cache.stored_at_unix_secs > now.saturating_add(CATALOG_FUTURE_CLOCK_SKEW_SECS) {
        u64::MAX
    } else {
        now.saturating_sub(cache.stored_at_unix_secs)
    }
}

fn catalog_cache_path(cache_root: &Path, connection_id: &str, fingerprint: &str) -> PathBuf {
    cache_root
        .join("provider-models")
        .join("v1")
        .join(connection_id)
        .join(format!("{fingerprint}.json"))
}

fn secure_catalog_tree(cache_root: &Path, connection_id: &str) -> Result<()> {
    validate_catalog_path_ancestors(cache_root)?;
    let cache_root_created = !cache_root.exists();
    if cache_root_created {
        fs::create_dir_all(cache_root)
            .with_context(|| format!("failed to create {}", cache_root.display()))?;
    }
    validate_catalog_path_ancestors(cache_root)?;
    validate_catalog_directory(cache_root, false)?;
    if cache_root_created {
        secure_private_path_permissions(cache_root)?;
    }
    let mut current = cache_root.to_path_buf();
    for component in ["provider-models", "v1", connection_id] {
        validate_catalog_directory(&current, false)?;
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(_) => validate_catalog_directory(&current, false)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                validate_catalog_path_ancestors(&current)?;
                match fs::create_dir(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        // Another workspace server may be publishing the same connection-scoped
                        // cache tree. Treat that race as success only after validating the winner.
                        validate_catalog_directory(&current, false)?;
                    }
                    Err(error) => {
                        return Err(error)
                            .with_context(|| format!("failed to create {}", current.display()));
                    }
                }
                validate_catalog_path_ancestors(&current)?;
            }
            Err(error) => return Err(error.into()),
        }
        secure_private_path_permissions(&current)?;
    }
    secure_private_path_permissions(&current)?;
    Ok(())
}

fn validate_catalog_tree(cache_root: &Path, connection_id: &str) -> Result<()> {
    validate_catalog_path_ancestors(cache_root)?;
    let mut current = cache_root.to_path_buf();
    validate_catalog_directory(&current, false)?;
    for component in ["provider-models", "v1", connection_id] {
        current.push(component);
        validate_catalog_directory(&current, true)?;
    }
    Ok(())
}

fn remove_catalog_cache_file(
    cache_root: &Path,
    connection_id: &str,
    fingerprint: &str,
) -> Result<()> {
    let file_name = format!("{fingerprint}.json");
    remove_catalog_cache_entry(cache_root, connection_id, std::ffi::OsStr::new(&file_name))
}

fn remove_catalog_cache_entry(
    cache_root: &Path,
    connection_id: &str,
    file_name: &std::ffi::OsStr,
) -> Result<()> {
    validate_catalog_tree(cache_root, connection_id)?;
    let parent = cache_root
        .join("provider-models")
        .join("v1")
        .join(connection_id);
    #[cfg(unix)]
    {
        use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};

        let directory = open_catalog_directory_no_follow(&parent)?;
        let name = std::ffi::CString::new(file_name.as_bytes())
            .context("catalog cache file name contains a NUL byte")?;
        // SAFETY: directory owns a valid descriptor and name is a relative C string.
        let result = unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) };
        if result == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            Ok(())
        } else {
            Err(error).with_context(|| {
                format!(
                    "failed to remove catalog cache {}",
                    parent.join(file_name).display()
                )
            })
        }
    }
    #[cfg(not(unix))]
    {
        let path = parent.join(file_name);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error)
                .with_context(|| format!("failed to remove catalog cache {}", path.display())),
        }
    }
}

#[cfg(unix)]
fn open_catalog_directory_no_follow(path: &Path) -> Result<fs::File> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    };

    let walk_path = catalog_directory_walk_path(path)?;
    let mut directory = if walk_path.is_absolute() {
        fs::File::open("/").context("failed to open filesystem root for catalog cache")?
    } else {
        fs::File::open(".").context("failed to open current directory for catalog cache")?
    };
    for component in walk_path.components() {
        let name = match component {
            std::path::Component::RootDir | std::path::Component::CurDir => continue,
            std::path::Component::ParentDir => std::ffi::OsStr::new(".."),
            std::path::Component::Normal(name) => name,
            std::path::Component::Prefix(_) => {
                anyhow::bail!(
                    "unsupported Unix catalog cache prefix {}",
                    walk_path.display()
                );
            }
        };
        let name = std::ffi::CString::new(name.as_bytes())
            .context("catalog cache component contains a NUL byte")?;
        // SAFETY: directory owns a valid descriptor and name is a relative C string.
        let descriptor = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| {
                format!(
                    "failed to open catalog cache component without following links: {}",
                    path.display()
                )
            });
        }
        // SAFETY: descriptor was returned by openat and transfers to File exactly once.
        directory = unsafe { fs::File::from_raw_fd(descriptor) };
    }
    Ok(directory)
}

#[cfg(unix)]
fn catalog_directory_walk_path(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let mut components = path.components();
    let Some(std::path::Component::RootDir) = components.next() else {
        return Ok(path.to_path_buf());
    };
    let Some(first) = components.next() else {
        return Ok(path.to_path_buf());
    };
    let std::path::Component::Normal(first_name) = first else {
        return Ok(path.to_path_buf());
    };
    let first_path = Path::new("/").join(first_name);
    let metadata = fs::symlink_metadata(&first_path)
        .with_context(|| format!("failed to inspect {}", first_path.display()))?;
    if !metadata.file_type().is_symlink() {
        return Ok(path.to_path_buf());
    }
    let mut resolved = first_path.canonicalize().with_context(|| {
        format!(
            "failed to resolve root-level alias {}",
            first_path.display()
        )
    })?;
    anyhow::ensure!(
        resolved.is_absolute(),
        "root-level catalog cache alias did not resolve to an absolute directory"
    );
    for component in components {
        resolved.push(component.as_os_str());
    }
    Ok(resolved)
}

fn validate_catalog_path_ancestors(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {}", current.display()));
            }
        };
        if metadata.file_type().is_symlink() {
            let is_root_level_alias = current
                .parent()
                .is_some_and(|ancestor| ancestor == Path::new("/"));
            anyhow::ensure!(
                cfg!(unix) && is_root_level_alias,
                "refusing to traverse symbolic-link catalog cache ancestor {}",
                current.display()
            );
            continue;
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;

            const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
            anyhow::ensure!(
                metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
                "catalog cache ancestor contains a Windows reparse point"
            );
        }
        anyhow::ensure!(
            metadata.is_dir() || current == path,
            "catalog cache ancestor is not a directory: {}",
            current.display()
        );
    }
    Ok(())
}

fn validate_catalog_directory(path: &Path, require_private_mode: bool) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    anyhow::ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "catalog cache path is not a private directory"
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        anyhow::ensure!(
            metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
            "catalog cache path contains a Windows reparse point"
        );
    }
    #[cfg(unix)]
    if require_private_mode {
        use std::os::unix::fs::PermissionsExt;

        anyhow::ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "catalog cache path permissions are not private"
        );
    }
    #[cfg(not(unix))]
    let _ = require_private_mode;
    Ok(())
}

fn catalog_file_metadata_is_safe(metadata: &fs::Metadata) -> bool {
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return false;
        }
    }
    true
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
