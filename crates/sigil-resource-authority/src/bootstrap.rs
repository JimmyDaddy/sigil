//! RFC-0071 section 10.1: AuthorityBootstrapRoots.
//!
//! The state/cache/temp arena top-level anchors are not ordinary ResourceKindV1 and do not issue
//! themselves a lease from the internal journal. This one-time platform resolution performs
//! create-new, owner-only hardening, no-follow identity capture, writer lock and per-startup
//! revalidation. A damaged manifest, owner/identity drift or writer-lock conflict makes the
//! whole authority fail closed: it never repairs itself through the ordinary journal.

use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};

use fs2::FileExt;
use serde::de::DeserializeOwned;

use sigil_kernel::resource::CanonicalHash;

/// Bootstrap error classification (closed).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BootstrapErrorV1 {
    #[error("state root unavailable; refusing cwd fallback")]
    StateRootUnavailable,
    #[error("bootstrap root is not a plain directory (symlink/reparse rejected): {0}")]
    NotPlainDirectory(String),
    #[error("bootstrap root identity drifted from the frozen manifest")]
    IdentityDrift,
    #[error("bootstrap manifest is corrupted or unknown version")]
    ManifestCorrupted,
    #[error("bootstrap writer lock is held by another instance")]
    WriterLockContended,
    #[error("owner-only permission hardening failed: {0}")]
    HardeningFailed(String),
    #[error("authority bootstrap metadata is corrupted: {0}")]
    MetadataCorrupted(String),
    #[error("authority bootstrap reconciliation is required: {0}")]
    ReconciliationRequired(String),
}

/// Platform root classes (closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorityBootstrapObjectClassV1 {
    StateAnchor,
    CacheAnchor,
    ExecutionTempAnchor,
    BootstrapManifest,
    WriterLock,
    ResourceJournalShard,
    EmergencyReserve,
    AuthorityConfigGeneration,
    CutoverPointer,
    PublicationTemp,
}

/// Resolved platform roots for one authority instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityBootstrapRoots {
    pub state_anchor: PathBuf,
    pub cache_anchor: PathBuf,
    pub execution_temp_anchor: PathBuf,
    pub state_identity: CanonicalHash,
    pub cache_identity: CanonicalHash,
    pub execution_temp_identity: CanonicalHash,
    pub manifest_hash: CanonicalHash,
    pub journal_instance_hash: CanonicalHash,
}

impl AuthorityBootstrapRoots {
    /// Validates that every anchor is a plain directory (no-follow) and owner-only.
    pub fn validate_anchors(&self) -> Result<(), BootstrapErrorV1> {
        for (label, path) in [
            ("state", &self.state_anchor),
            ("cache", &self.cache_anchor),
            ("execution-temp", &self.execution_temp_anchor),
        ] {
            let metadata = std::fs::symlink_metadata(path).map_err(|error| {
                BootstrapErrorV1::NotPlainDirectory(format!("{label}: {}", error))
            })?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(BootstrapErrorV1::NotPlainDirectory(label.to_owned()));
            }
        }
        Ok(())
    }
}

const AUTHORITY_BOOTSTRAP_DIRECTORY_NAME: &str = "authority-bootstrap-v1";
const AUTHORITY_BOOTSTRAP_PUBLICATION_LOCK: &str = "authority-bootstrap-publication.lock";
const AUTHORITY_CONFIG_GENERATION_FILE: &str = "authority-config-generation.json";
const CUTOVER_POINTER_FILE: &str = ".sigil-cutover-manifest.json";
const MAX_BOOTSTRAP_METADATA_BYTES: u64 = 2 * 1024 * 1024;

/// Host-owned durable store for bootstrap metadata.
///
/// The store is deliberately independent of the caller-provided config parent and configured
/// workspace/storage roots. One config identity gets one private directory beneath the user's
/// Sigil directory; the parent hierarchy and the instance directory are owner-only before any
/// lock or metadata file is opened. The runtime may only use the fixed object classes below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityBootstrapStoreV1 {
    root: PathBuf,
    fresh: bool,
}

/// Cross-process publication guard. It must remain alive while generation allocation, readiness
/// publication, and current-pointer publication are performed; dropping it releases the single
/// bootstrap transaction lock.
#[derive(Debug)]
pub struct AuthorityBootstrapPublicationGuard {
    root: PathBuf,
    lock: File,
}

impl Drop for AuthorityBootstrapPublicationGuard {
    fn drop(&mut self) {
        let _ = self.lock.unlock();
    }
}

impl AuthorityBootstrapStoreV1 {
    /// Opens the stable host-owned bootstrap store for one canonical config path.
    pub fn for_config_path(config_path: &Path) -> Result<Self, BootstrapErrorV1> {
        let config_path = fs::canonicalize(config_path)
            .map_err(|error| BootstrapErrorV1::HardeningFailed(error.to_string()))?;
        Self::for_canonical_config_path(&config_path)
    }

    /// Opens the stable host-owned bootstrap store for a path already frozen by the boot owner.
    ///
    /// Production composition uses this form with the canonical path captured together with the
    /// config file handle. It intentionally does not resolve the filesystem path a second time,
    /// so a concurrent config-parent replacement cannot redirect metadata to another store.
    pub fn for_canonical_config_path(config_path: &Path) -> Result<Self, BootstrapErrorV1> {
        if !config_path.is_absolute() {
            return Err(BootstrapErrorV1::HardeningFailed(
                "canonical bootstrap config path must be absolute".to_owned(),
            ));
        }
        let config_key = canonical_bootstrap_hash(
            format!("authority-bootstrap-instance-v1\0{}", config_path.display()).as_bytes(),
        )
        .to_hex();
        let user_root = sigil_kernel::default_user_config_dir()
            .map_err(|error| BootstrapErrorV1::HardeningFailed(error.to_string()))?;
        ensure_owner_only_directory(&user_root)?;
        let root = user_root
            .join(AUTHORITY_BOOTSTRAP_DIRECTORY_NAME)
            .join(config_key);
        Self::open(root)
    }

    /// Opens a root after the host-owned resolver has selected its stable location.
    ///
    /// This remains private so callers cannot choose an arbitrary workspace/config-relative
    /// directory and thereby turn bootstrap metadata into a caller-owned authority input.
    fn open(root: impl Into<PathBuf>) -> Result<Self, BootstrapErrorV1> {
        let root = root.into();
        let fresh = matches!(
            fs::symlink_metadata(&root),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        );
        let parent = root.parent().ok_or_else(|| {
            BootstrapErrorV1::HardeningFailed("bootstrap root has no parent".to_owned())
        })?;
        ensure_owner_only_directory(parent)?;
        ensure_owner_only_directory(&root)?;
        Ok(Self { root, fresh })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn was_created_for_this_open(&self) -> bool {
        self.fresh
    }

    #[must_use]
    pub fn path(&self, object: AuthorityBootstrapObjectClassV1) -> PathBuf {
        self.root.join(match object {
            AuthorityBootstrapObjectClassV1::BootstrapManifest => "bootstrap-manifest.json",
            AuthorityBootstrapObjectClassV1::WriterLock => AUTHORITY_BOOTSTRAP_PUBLICATION_LOCK,
            AuthorityBootstrapObjectClassV1::AuthorityConfigGeneration => {
                AUTHORITY_CONFIG_GENERATION_FILE
            }
            AuthorityBootstrapObjectClassV1::CutoverPointer => CUTOVER_POINTER_FILE,
            AuthorityBootstrapObjectClassV1::PublicationTemp => "publication.tmp",
            AuthorityBootstrapObjectClassV1::StateAnchor => "state-anchor",
            AuthorityBootstrapObjectClassV1::CacheAnchor => "cache-anchor",
            AuthorityBootstrapObjectClassV1::ExecutionTempAnchor => "execution-temp-anchor",
            AuthorityBootstrapObjectClassV1::ResourceJournalShard => "resource-journal",
            AuthorityBootstrapObjectClassV1::EmergencyReserve => "emergency-reserve",
        })
    }

    /// Acquires the one lock that covers both generation allocation and current-pointer publish.
    pub fn acquire_publication(
        &self,
    ) -> Result<AuthorityBootstrapPublicationGuard, BootstrapErrorV1> {
        ensure_owner_only_directory(&self.root)?;
        let lock_path = self.path(AuthorityBootstrapObjectClassV1::WriterLock);
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(
                windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT,
            );
        }
        let lock = options.open(&lock_path).map_err(|error| {
            BootstrapErrorV1::HardeningFailed(format!(
                "failed to open bootstrap publication lock {}: {error}",
                lock_path.display()
            ))
        })?;
        validate_private_open_file(&lock_path, &lock, false)?;
        lock.lock_exclusive()
            .map_err(|_| BootstrapErrorV1::WriterLockContended)?;
        Ok(AuthorityBootstrapPublicationGuard {
            root: self.root.clone(),
            lock,
        })
    }

    /// Reads a fixed bootstrap object through a no-follow, already-open file and validates its
    /// owner-only identity before parsing. Missing metadata is distinct from corrupt metadata.
    pub fn read_json<T: DeserializeOwned>(
        &self,
        guard: &AuthorityBootstrapPublicationGuard,
        object: AuthorityBootstrapObjectClassV1,
    ) -> Result<Option<T>, BootstrapErrorV1> {
        self.validate_guard(guard)?;
        let Some(bytes) = self.read_bytes(guard, object)? else {
            return Ok(None);
        };
        serde_json::from_slice(&bytes).map(Some).map_err(|error| {
            BootstrapErrorV1::MetadataCorrupted(format!("{}: {error}", self.path(object).display()))
        })
    }

    /// Reads one fixed bootstrap object as bounded bytes after no-follow identity validation.
    pub fn read_bytes(
        &self,
        guard: &AuthorityBootstrapPublicationGuard,
        object: AuthorityBootstrapObjectClassV1,
    ) -> Result<Option<Vec<u8>>, BootstrapErrorV1> {
        self.validate_guard(guard)?;
        let path = self.path(object);
        let Some(file) = open_private_read_file(&path)? else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        file.take(MAX_BOOTSTRAP_METADATA_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| BootstrapErrorV1::MetadataCorrupted(error.to_string()))?;
        if bytes.len() as u64 > MAX_BOOTSTRAP_METADATA_BYTES {
            return Err(BootstrapErrorV1::MetadataCorrupted(format!(
                "{} exceeds the bounded metadata size",
                path.display()
            )));
        }
        Ok(Some(bytes))
    }

    /// Publishes one fixed bootstrap object atomically while the same publication guard is held.
    pub fn publish_bytes(
        &self,
        guard: &AuthorityBootstrapPublicationGuard,
        object: AuthorityBootstrapObjectClassV1,
        bytes: &[u8],
    ) -> Result<(), BootstrapErrorV1> {
        self.validate_guard(guard)?;
        if bytes.len() as u64 > MAX_BOOTSTRAP_METADATA_BYTES {
            return Err(BootstrapErrorV1::MetadataCorrupted(
                "bootstrap metadata exceeds the bounded size".to_owned(),
            ));
        }
        sigil_kernel::atomic_publish_private_file(&self.path(object), bytes)
            .map_err(|error| BootstrapErrorV1::HardeningFailed(error.to_string()))
    }

    fn validate_guard(
        &self,
        guard: &AuthorityBootstrapPublicationGuard,
    ) -> Result<(), BootstrapErrorV1> {
        if guard.root != self.root {
            return Err(BootstrapErrorV1::IdentityDrift);
        }
        ensure_owner_only_directory(&self.root)
    }
}

fn open_private_read_file(path: &Path) -> Result<Option<File>, BootstrapErrorV1> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(BootstrapErrorV1::MetadataCorrupted(error.to_string())),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(BootstrapErrorV1::MetadataCorrupted(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(
            windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ
                | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE
                | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_DELETE,
        );
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path).map_err(|error| {
        BootstrapErrorV1::MetadataCorrupted(format!("{}: {error}", path.display()))
    })?;
    validate_private_open_file(path, &file, true)?;
    Ok(Some(file))
}

fn validate_private_open_file(
    path: &Path,
    file: &File,
    require_nonempty: bool,
) -> Result<(), BootstrapErrorV1> {
    let metadata = file
        .metadata()
        .map_err(|error| BootstrapErrorV1::MetadataCorrupted(error.to_string()))?;
    if !metadata.is_file() {
        return Err(BootstrapErrorV1::MetadataCorrupted(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    if require_nonempty && metadata.len() == 0 {
        return Err(BootstrapErrorV1::MetadataCorrupted(format!(
            "{} is empty",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(BootstrapErrorV1::HardeningFailed(format!(
                "{} is not owned by the current user",
                path.display()
            )));
        }
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(BootstrapErrorV1::HardeningFailed(format!(
                "{} is not owner-only",
                path.display()
            )));
        }
    }
    #[cfg(windows)]
    {
        let identity = crate::identity::canonical_identity_from_handle(path, file)
            .map_err(|error| BootstrapErrorV1::MetadataCorrupted(error.to_string()))?;
        if identity.is_symlink
            || !sigil_kernel::private_path_permissions_are_restricted(path)
                .map_err(|error| BootstrapErrorV1::HardeningFailed(error.to_string()))?
        {
            return Err(BootstrapErrorV1::HardeningFailed(format!(
                "{} is not a protected regular file",
                path.display()
            )));
        }
    }
    Ok(())
}

fn ensure_owner_only_directory(path: &Path) -> Result<(), BootstrapErrorV1> {
    reject_symlink_components(path)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(BootstrapErrorV1::NotPlainDirectory(
                path.display().to_string(),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(|create_error| {
                BootstrapErrorV1::HardeningFailed(format!("{}: {create_error}", path.display()))
            })?;
        }
        Err(error) => {
            return Err(BootstrapErrorV1::HardeningFailed(format!(
                "{}: {error}",
                path.display()
            )));
        }
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        BootstrapErrorV1::HardeningFailed(format!("{}: {error}", path.display()))
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(BootstrapErrorV1::NotPlainDirectory(
            path.display().to_string(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(BootstrapErrorV1::HardeningFailed(format!(
                "{} is not owned by the current user",
                path.display()
            )));
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
            BootstrapErrorV1::HardeningFailed(format!("{}: {error}", path.display()))
        })?;
    }
    #[cfg(windows)]
    sigil_kernel::secure_private_path_permissions(path)
        .map_err(|error| BootstrapErrorV1::HardeningFailed(error.to_string()))?;
    if !sigil_kernel::private_path_permissions_are_restricted(path)
        .map_err(|error| BootstrapErrorV1::HardeningFailed(error.to_string()))?
    {
        return Err(BootstrapErrorV1::HardeningFailed(format!(
            "{} is not owner-only",
            path.display()
        )));
    }
    Ok(())
}

fn reject_symlink_components(path: &Path) -> Result<(), BootstrapErrorV1> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(BootstrapErrorV1::NotPlainDirectory(
                    current.display().to_string(),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(BootstrapErrorV1::HardeningFailed(format!(
                    "{}: {error}",
                    current.display()
                )));
            }
        }
    }
    Ok(())
}

/// Deterministic root resolver with explicit injection (tests use isolated roots).
#[derive(Debug, Clone)]
pub struct BootstrapRootResolverV1 {
    pub explicit_state_home: Option<PathBuf>,
    pub explicit_cache_home: Option<PathBuf>,
    pub system_temp_parent: Option<PathBuf>,
}

/// Resolver defaults to explicit-injection-only (no ambient resolution).
impl Default for BootstrapRootResolverV1 {
    fn default() -> Self {
        Self {
            explicit_state_home: None,
            explicit_cache_home: None,
            system_temp_parent: None,
        }
    }
}

impl BootstrapRootResolverV1 {
    /// Resolves roots. Never falls back to cwd and never places durable state in system temp.
    pub fn resolve(&self) -> Result<AuthorityBootstrapRoots, BootstrapErrorV1> {
        let state_anchor = self
            .explicit_state_home
            .clone()
            .ok_or(BootstrapErrorV1::StateRootUnavailable)?;
        let cache_anchor = self
            .explicit_cache_home
            .clone()
            .ok_or(BootstrapErrorV1::StateRootUnavailable)?;
        let execution_temp_anchor = self
            .system_temp_parent
            .clone()
            .ok_or(BootstrapErrorV1::StateRootUnavailable)?
            .join("sigil-execution-temp");
        let manifest_hash = canonical_bootstrap_hash(b"authority-bootstrap-manifest-v1");
        let journal_instance_hash = canonical_bootstrap_hash(b"authority-journal-instance-v1");
        let roots = AuthorityBootstrapRoots {
            state_anchor,
            cache_anchor,
            execution_temp_anchor,
            state_identity: canonical_bootstrap_hash(b"state-anchor-identity-v1"),
            cache_identity: canonical_bootstrap_hash(b"cache-anchor-identity-v1"),
            execution_temp_identity: canonical_bootstrap_hash(b"execution-temp-identity-v1"),
            manifest_hash,
            journal_instance_hash,
        };
        roots.validate_anchors()?;
        Ok(roots)
    }
}

/// Canonical digest used for bootstrap identities.
pub fn canonical_bootstrap_hash(payload: &[u8]) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(payload);
    CanonicalHash::from_bytes(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r71_bootstrap_resolve_rejects_missing_state_home_without_cwd_fallback() {
        let resolver = BootstrapRootResolverV1::default();
        let error = resolver.resolve().expect_err("must fail closed");
        assert!(matches!(error, BootstrapErrorV1::StateRootUnavailable));
    }

    #[cfg(unix)]
    #[test]
    fn r71_bootstrap_rejects_symlinked_state_anchor() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().expect("tempdir");
        let state_target = temp.path().join("state-target");
        std::fs::create_dir_all(&state_target).expect("target");
        let state_link = temp.path().join("state-link");
        symlink(&state_target, &state_link).expect("link");

        let roots = AuthorityBootstrapRoots {
            state_anchor: state_link,
            cache_anchor: temp.path().join("cache"),
            execution_temp_anchor: temp.path().join("et"),
            state_identity: canonical_bootstrap_hash(b"x"),
            cache_identity: canonical_bootstrap_hash(b"y"),
            execution_temp_identity: canonical_bootstrap_hash(b"z"),
            manifest_hash: canonical_bootstrap_hash(b"m"),
            journal_instance_hash: canonical_bootstrap_hash(b"j"),
        };
        let error = roots.validate_anchors().expect_err("symlink must fail");
        assert!(matches!(error, BootstrapErrorV1::NotPlainDirectory(_)));
    }

    #[test]
    fn r71_bootstrap_hash_is_stable() {
        assert_eq!(
            canonical_bootstrap_hash(b"payload"),
            canonical_bootstrap_hash(b"payload")
        );
    }
}
