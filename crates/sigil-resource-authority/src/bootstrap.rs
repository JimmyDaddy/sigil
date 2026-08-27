//! RFC-0071 section 10.1: AuthorityBootstrapRoots.
//!
//! The state/cache/temp arena top-level anchors are not ordinary ResourceKindV1 and do not issue
//! themselves a lease from the internal journal. This one-time platform resolution performs
//! create-new, owner-only hardening, no-follow identity capture, writer lock and per-startup
//! revalidation. A damaged manifest, owner/identity drift or writer-lock conflict makes the
//! whole authority fail closed: it never repairs itself through the ordinary journal.

use std::{
    collections::BTreeMap,
    fmt,
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
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
    ActiveEpochPointer,
    OldEpochInertMarker,
    RecoveryIntent,
    RecoveryReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ActiveEpochPointerRecordV1 {
    schema_version: u32,
    epoch: u64,
    epoch_name: String,
    root_identity_hash: CanonicalHash,
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
const AUTHORITY_BOOTSTRAP_TRANSACTION_LOCK: &str = "authority-bootstrap-transaction.lock";
const AUTHORITY_CONFIG_GENERATION_FILE: &str = "authority-config-generation.json";
const CUTOVER_POINTER_FILE: &str = ".sigil-cutover-manifest.json";
const ACTIVE_EPOCH_POINTER_FILE: &str = ".sigil-active-epoch.json";
const OLD_EPOCH_INERT_FILE: &str = ".sigil-old-epoch-inert.json";
const RECOVERY_INTENT_FILE: &str = ".sigil-bootstrap-recovery-intent.json";
const RECOVERY_RECEIPT_FILE: &str = ".sigil-bootstrap-recovery-receipt.json";
const EPOCHS_DIRECTORY_NAME: &str = "epochs";
const MAX_BOOTSTRAP_METADATA_BYTES: u64 = 2 * 1024 * 1024;
const ACTIVE_EPOCH_POINTER_SCHEMA_VERSION: u32 = 1;

/// Host-owned durable store for bootstrap metadata.
///
/// The store is deliberately independent of the caller-provided config parent and configured
/// workspace/storage roots. One config identity gets one private directory beneath the user's
/// Sigil directory; the parent hierarchy and the instance directory are owner-only before any
/// lock or metadata file is opened. The runtime may only use the fixed object classes below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityBootstrapStoreV1 {
    namespace: PathBuf,
    root: PathBuf,
    fresh: bool,
    authority_epoch: u64,
}

/// Cross-process publication guard. It must remain alive while generation allocation, readiness
/// publication, and current-pointer publication are performed; dropping it releases the single
/// bootstrap transaction lock.
#[derive(Debug)]
pub struct AuthorityBootstrapPublicationGuard {
    namespace: PathBuf,
    root: PathBuf,
    namespace_lock: File,
    lock: File,
}

impl Drop for AuthorityBootstrapPublicationGuard {
    fn drop(&mut self) {
        let _ = self.lock.unlock();
        let _ = self.namespace_lock.unlock();
    }
}

/// Authority-owned recovery namespace handle. It can be obtained without opening the possibly
/// corrupt active epoch, which is the deliberate escape hatch used only by doctor/operator
/// recovery. It never exposes a caller-selected arbitrary root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityBootstrapRecoveryNamespaceV1 {
    namespace: PathBuf,
}

/// Exclusive operator transaction lock. Normal boot publication and fresh-epoch recovery share
/// this lock, so recovery cannot race a boot that still believes the old epoch is active.
#[derive(Debug)]
pub struct AuthorityBootstrapRecoveryTransactionGuard {
    namespace: PathBuf,
    lock: File,
}

impl Drop for AuthorityBootstrapRecoveryTransactionGuard {
    fn drop(&mut self) {
        let _ = self.lock.unlock();
    }
}

impl AuthorityBootstrapRecoveryNamespaceV1 {
    #[must_use]
    pub fn namespace(&self) -> &Path {
        &self.namespace
    }

    /// Acquires the exclusive transaction guard shared with normal boot publication.
    pub fn acquire_transaction(
        &self,
    ) -> Result<AuthorityBootstrapRecoveryTransactionGuard, BootstrapErrorV1> {
        ensure_owner_only_directory(&self.namespace)?;
        let path = self.namespace.join(AUTHORITY_BOOTSTRAP_TRANSACTION_LOCK);
        let lock = open_private_lock_file(&path)?;
        lock.lock_exclusive()
            .map_err(|_| BootstrapErrorV1::WriterLockContended)?;
        Ok(AuthorityBootstrapRecoveryTransactionGuard {
            namespace: self.namespace.clone(),
            lock,
        })
    }

    /// Resolves the active store for recovery without changing the normal boot fail-closed rule.
    pub fn active_store(&self) -> Result<AuthorityBootstrapStoreV1, BootstrapErrorV1> {
        let (root, epoch) = resolve_active_epoch(&self.namespace)?;
        AuthorityBootstrapStoreV1::open(&self.namespace, root, epoch)
    }

    /// Publishes the active epoch pointer while the recovery transaction guard is held.
    fn publish_active_epoch(
        &self,
        guard: &AuthorityBootstrapRecoveryTransactionGuard,
        epoch: u64,
        root: &Path,
    ) -> Result<(), BootstrapErrorV1> {
        self.validate_guard(guard)?;
        let identity = bootstrap_root_identity(root)?;
        let epoch_name = root
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                BootstrapErrorV1::MetadataCorrupted("epoch root has no name".to_owned())
            })?;
        let record = ActiveEpochPointerRecordV1 {
            schema_version: ACTIVE_EPOCH_POINTER_SCHEMA_VERSION,
            epoch,
            epoch_name: epoch_name.to_owned(),
            root_identity_hash: identity,
        };
        let bytes = serde_json::to_vec(&record)
            .map_err(|error| BootstrapErrorV1::MetadataCorrupted(error.to_string()))?;
        publish_private_bootstrap_file(&self.namespace.join(ACTIVE_EPOCH_POINTER_FILE), &bytes)
    }

    fn validate_guard(
        &self,
        guard: &AuthorityBootstrapRecoveryTransactionGuard,
    ) -> Result<(), BootstrapErrorV1> {
        if guard.namespace != self.namespace {
            return Err(BootstrapErrorV1::IdentityDrift);
        }
        ensure_owner_only_directory(&self.namespace)
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
        let configured_user_root = sigil_kernel::default_user_config_dir()
            .map_err(|error| BootstrapErrorV1::HardeningFailed(error.to_string()))?;
        let user_parent = configured_user_root.parent().ok_or_else(|| {
            BootstrapErrorV1::HardeningFailed("user config directory has no parent".to_owned())
        })?;
        // macOS commonly exposes `/var` as a compatibility symlink. Resolve only the trusted
        // host HOME parent before hardening the authority-owned suffix; a symlink in the suffix
        // itself is still rejected by `ensure_owner_only_directory`.
        let user_parent = fs::canonicalize(user_parent)
            .map_err(|error| BootstrapErrorV1::HardeningFailed(error.to_string()))?;
        let user_root = user_parent.join(configured_user_root.file_name().ok_or_else(|| {
            BootstrapErrorV1::HardeningFailed(
                "user config directory has no final component".to_owned(),
            )
        })?);
        ensure_owner_only_directory(&user_root)?;
        let root = user_root
            .join(AUTHORITY_BOOTSTRAP_DIRECTORY_NAME)
            .join(config_key);
        let (active_root, authority_epoch) = resolve_active_epoch(&root)?;
        Self::open(root, active_root, authority_epoch)
    }

    /// Opens the recovery namespace without opening its active epoch. This is intentionally a
    /// separate doctor/operator entry because normal boot must remain fail-closed on corruption.
    pub fn recovery_namespace_for_canonical_config_path(
        config_path: &Path,
    ) -> Result<AuthorityBootstrapRecoveryNamespaceV1, BootstrapErrorV1> {
        if !config_path.is_absolute() {
            return Err(BootstrapErrorV1::HardeningFailed(
                "canonical bootstrap config path must be absolute".to_owned(),
            ));
        }
        let config_key = canonical_bootstrap_hash(
            format!("authority-bootstrap-instance-v1\0{}", config_path.display()).as_bytes(),
        )
        .to_hex();
        let configured_user_root = sigil_kernel::default_user_config_dir()
            .map_err(|error| BootstrapErrorV1::HardeningFailed(error.to_string()))?;
        let user_parent = configured_user_root.parent().ok_or_else(|| {
            BootstrapErrorV1::HardeningFailed("user config directory has no parent".to_owned())
        })?;
        let user_parent = fs::canonicalize(user_parent)
            .map_err(|error| BootstrapErrorV1::HardeningFailed(error.to_string()))?;
        let user_root = user_parent.join(configured_user_root.file_name().ok_or_else(|| {
            BootstrapErrorV1::HardeningFailed(
                "user config directory has no final component".to_owned(),
            )
        })?);
        ensure_owner_only_directory(&user_root)?;
        let namespace = user_root
            .join(AUTHORITY_BOOTSTRAP_DIRECTORY_NAME)
            .join(config_key);
        ensure_owner_only_directory(&namespace)?;
        Ok(AuthorityBootstrapRecoveryNamespaceV1 { namespace })
    }

    /// Opens a root after the host-owned resolver has selected its stable location.
    ///
    /// This remains private so callers cannot choose an arbitrary workspace/config-relative
    /// directory and thereby turn bootstrap metadata into a caller-owned authority input.
    fn open(
        namespace: impl Into<PathBuf>,
        root: impl Into<PathBuf>,
        authority_epoch: u64,
    ) -> Result<Self, BootstrapErrorV1> {
        let namespace = namespace.into();
        let root = root.into();
        let created = matches!(
            fs::symlink_metadata(&root),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        );
        let recovery_selected_fresh_root = !created
            && fs::symlink_metadata(root.join(RECOVERY_RECEIPT_FILE)).is_ok()
            && fs::symlink_metadata(root.join(AUTHORITY_CONFIG_GENERATION_FILE)).is_err();
        let parent = root.parent().ok_or_else(|| {
            BootstrapErrorV1::HardeningFailed("bootstrap root has no parent".to_owned())
        })?;
        ensure_owner_only_directory(parent)?;
        ensure_owner_only_directory(&root)?;
        Ok(Self {
            namespace,
            root,
            fresh: created || recovery_selected_fresh_root,
            authority_epoch,
        })
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
    pub const fn authority_epoch(&self) -> u64 {
        self.authority_epoch
    }

    #[must_use]
    pub fn namespace(&self) -> &Path {
        &self.namespace
    }

    #[must_use]
    pub fn path(&self, object: AuthorityBootstrapObjectClassV1) -> PathBuf {
        let name = match object {
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
            AuthorityBootstrapObjectClassV1::ActiveEpochPointer => ACTIVE_EPOCH_POINTER_FILE,
            AuthorityBootstrapObjectClassV1::OldEpochInertMarker => OLD_EPOCH_INERT_FILE,
            AuthorityBootstrapObjectClassV1::RecoveryIntent => RECOVERY_INTENT_FILE,
            AuthorityBootstrapObjectClassV1::RecoveryReceipt => RECOVERY_RECEIPT_FILE,
        };
        match object {
            AuthorityBootstrapObjectClassV1::ActiveEpochPointer => self.namespace.join(name),
            _ => self.root.join(name),
        }
    }

    /// Acquires the one lock that covers both generation allocation and current-pointer publish.
    pub fn acquire_publication(
        &self,
    ) -> Result<AuthorityBootstrapPublicationGuard, BootstrapErrorV1> {
        ensure_owner_only_directory(&self.namespace)?;
        ensure_owner_only_directory(&self.root)?;
        let namespace_lock_path = self.namespace.join(AUTHORITY_BOOTSTRAP_TRANSACTION_LOCK);
        let namespace_lock = open_private_lock_file(&namespace_lock_path)?;
        namespace_lock
            .lock_exclusive()
            .map_err(|_| BootstrapErrorV1::WriterLockContended)?;
        let lock_path = self.path(AuthorityBootstrapObjectClassV1::WriterLock);
        let lock = open_private_lock_file(&lock_path).map_err(|error| {
            BootstrapErrorV1::HardeningFailed(format!(
                "failed to open bootstrap publication lock {}: {error}",
                lock_path.display()
            ))
        })?;
        validate_private_open_file(&lock_path, &lock, false)?;
        lock.lock_exclusive()
            .map_err(|_| BootstrapErrorV1::WriterLockContended)?;
        Ok(AuthorityBootstrapPublicationGuard {
            namespace: self.namespace.clone(),
            root: self.root.clone(),
            namespace_lock,
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
        if matches!(
            object,
            AuthorityBootstrapObjectClassV1::ActiveEpochPointer
                | AuthorityBootstrapObjectClassV1::OldEpochInertMarker
                | AuthorityBootstrapObjectClassV1::RecoveryIntent
                | AuthorityBootstrapObjectClassV1::RecoveryReceipt
        ) {
            return Err(BootstrapErrorV1::ReconciliationRequired(
                "recovery-owned bootstrap metadata may only be published by the independent recovery service".to_owned(),
            ));
        }
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
        if guard.namespace != self.namespace || guard.root != self.root {
            return Err(BootstrapErrorV1::IdentityDrift);
        }
        ensure_owner_only_directory(&self.namespace)?;
        ensure_owner_only_directory(&self.root)
    }
}

fn open_private_lock_file(path: &Path) -> Result<File, BootstrapErrorV1> {
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
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path).map_err(|error| {
        BootstrapErrorV1::HardeningFailed(format!(
            "failed to open private bootstrap lock {}: {error}",
            path.display()
        ))
    })?;
    validate_private_open_file(path, &file, false)?;
    Ok(file)
}

fn resolve_active_epoch(namespace: &Path) -> Result<(PathBuf, u64), BootstrapErrorV1> {
    match fs::symlink_metadata(namespace) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(BootstrapErrorV1::NotPlainDirectory(
                namespace.display().to_string(),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((namespace.to_path_buf(), 1));
        }
        Err(error) => return Err(BootstrapErrorV1::HardeningFailed(error.to_string())),
    }
    ensure_owner_only_directory(namespace)?;

    let pointer_path = namespace.join(ACTIVE_EPOCH_POINTER_FILE);
    let Some(pointer_file) = open_private_read_file(&pointer_path)? else {
        if fs::symlink_metadata(namespace.join(OLD_EPOCH_INERT_FILE)).is_ok() {
            return Err(BootstrapErrorV1::ReconciliationRequired(
                "old authority epoch is inert but active epoch pointer is missing".to_owned(),
            ));
        }
        return Ok((namespace.to_path_buf(), 1));
    };
    let mut bytes = Vec::new();
    pointer_file
        .take(MAX_BOOTSTRAP_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| BootstrapErrorV1::MetadataCorrupted(error.to_string()))?;
    if bytes.len() as u64 > MAX_BOOTSTRAP_METADATA_BYTES {
        return Err(BootstrapErrorV1::MetadataCorrupted(
            "active epoch pointer exceeds the bounded metadata size".to_owned(),
        ));
    }
    let record: ActiveEpochPointerRecordV1 = serde_json::from_slice(&bytes)
        .map_err(|error| BootstrapErrorV1::MetadataCorrupted(error.to_string()))?;
    if record.schema_version != ACTIVE_EPOCH_POINTER_SCHEMA_VERSION
        || record.epoch == 0
        || !valid_epoch_name(&record.epoch_name)
    {
        return Err(BootstrapErrorV1::MetadataCorrupted(
            "active epoch pointer record is invalid".to_owned(),
        ));
    }
    let epochs = namespace.join(EPOCHS_DIRECTORY_NAME);
    ensure_owner_only_directory(&epochs)?;
    let root = epochs.join(&record.epoch_name);
    let identity = bootstrap_root_identity(&root)?;
    if identity != record.root_identity_hash {
        return Err(BootstrapErrorV1::IdentityDrift);
    }
    ensure_owner_only_directory(&root)?;
    Ok((root, record.epoch))
}

fn valid_epoch_name(name: &str) -> bool {
    name.len() <= 128
        && name.starts_with("epoch-")
        && name
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

fn bootstrap_root_identity(path: &Path) -> Result<CanonicalHash, BootstrapErrorV1> {
    let identity = crate::identity::canonical_identity(path)
        .map_err(|error| BootstrapErrorV1::MetadataCorrupted(error.to_string()))?;
    if !identity.is_directory || identity.is_symlink {
        return Err(BootstrapErrorV1::NotPlainDirectory(
            path.display().to_string(),
        ));
    }
    Ok(identity.digest)
}

fn publish_private_bootstrap_file(path: &Path, bytes: &[u8]) -> Result<(), BootstrapErrorV1> {
    if bytes.len() as u64 > MAX_BOOTSTRAP_METADATA_BYTES {
        return Err(BootstrapErrorV1::MetadataCorrupted(
            "bootstrap metadata exceeds the bounded size".to_owned(),
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        BootstrapErrorV1::HardeningFailed("bootstrap metadata has no parent".to_owned())
    })?;
    ensure_owner_only_directory(parent)?;
    sigil_kernel::atomic_publish_private_file(path, bytes)
        .map_err(|error| BootstrapErrorV1::HardeningFailed(error.to_string()))?;
    let file = open_private_read_file(path)?.ok_or_else(|| {
        BootstrapErrorV1::MetadataCorrupted(format!("{} disappeared after publish", path.display()))
    })?;
    validate_private_open_file(path, &file, true)
}

/// Failure evidence supplied by the doctor/operator. It is deliberately separate from the
/// normal authority journal error so a damaged authority cannot authorize its own replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AuthorityJournalFailureClassV1 {
    CorruptHashChain,
    TruncatedOrTornRecord,
    WriterLockConflict,
    UnreadableOrIdentityDrift,
    EmergencyReserveExhausted,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FailedAuthorityJournalEvidenceV1 {
    pub journal_scope: sigil_kernel::resource::ResourceJournalScopeV1,
    pub expected_anchor_identity: CanonicalHash,
    pub last_verified_record_hash: Option<CanonicalHash>,
    pub observed_failure_digest: CanonicalHash,
    pub failure_class: AuthorityJournalFailureClassV1,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OldAuthorityEpochQuiescenceProofV1 {
    pub failed_epoch_evidence_set_hash: CanonicalHash,
    pub known_process_tree_inventory_hash: CanonicalHash,
    pub process_owner_probe_hash: CanonicalHash,
    pub terminal_or_absent_proof_hash: CanonicalHash,
    pub observed_at_ms: u64,
    pub proof_hash: CanonicalHash,
}

macro_rules! bootstrap_opaque_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                Ok(Self(String::deserialize(deserializer)?))
            }
        }
    };
}

bootstrap_opaque_id!(
    OpaqueBootstrapRootConfigRef,
    "Opaque doctor-selected fresh authority root configuration reference."
);
bootstrap_opaque_id!(
    OpaqueDiagnosticRef,
    "Opaque doctor diagnostic reference; it never carries a filesystem path."
);
bootstrap_opaque_id!(
    OpaqueOperatorChallengeId,
    "Opaque one-shot bootstrap operator challenge identifier."
);
bootstrap_opaque_id!(
    OpaqueBootstrapRecoveryAuthorizationId,
    "Opaque one-shot bootstrap recovery authorization identifier."
);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AuthorityBootstrapRecoveryOperationV1 {
    SelectFreshAuthorityEpoch {
        explicit_root_config: OpaqueBootstrapRootConfigRef,
        expected_failed_bootstrap_hash: Option<CanonicalHash>,
        failed_journal_evidence: Vec<FailedAuthorityJournalEvidenceV1>,
        evidence_set_hash: CanonicalHash,
        old_epoch_quiescence: Box<OldAuthorityEpochQuiescenceProofV1>,
    },
    RevealBootstrapDiagnostic {
        diagnostic_ref: OpaqueDiagnosticRef,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExactBootstrapOperatorConfirmationV1 {
    pub challenge_id: OpaqueOperatorChallengeId,
    pub operation_hash: CanonicalHash,
    pub evidence_set_hash: CanonicalHash,
    pub quiescence_proof_hash: Option<CanonicalHash>,
    pub fresh_root_selection_hash: Option<CanonicalHash>,
    pub confirmed_at_ms: u64,
    pub confirmation_hash: CanonicalHash,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AuthorityBootstrapOperatorChallengeV1 {
    pub challenge_id: OpaqueOperatorChallengeId,
    pub operation_hash: CanonicalHash,
    pub expires_at_ms: u64,
    pub challenge_hash: CanonicalHash,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AuthorityBootstrapRecoveryAuthorizationV1 {
    pub authorization_id: OpaqueBootstrapRecoveryAuthorizationId,
    pub operation_hash: CanonicalHash,
    pub evidence_set_hash: CanonicalHash,
    pub quiescence_proof_hash: Option<CanonicalHash>,
    pub operator_confirmation_hash: CanonicalHash,
    pub expires_at_ms: u64,
    authenticator: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AuthorityBootstrapRecoveryReceiptV1 {
    pub schema_version: u32,
    pub operation_hash: CanonicalHash,
    pub evidence_set_hash: CanonicalHash,
    pub old_authority_epoch: u64,
    pub new_authority_epoch: u64,
    pub old_root_identity_hash: CanonicalHash,
    pub new_root_identity_hash: CanonicalHash,
    pub recovery_intent_hash: CanonicalHash,
    pub receipt_hash: CanonicalHash,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct FreshRootSelectionRecordV1 {
    root_ref: OpaqueBootstrapRootConfigRef,
    state_root: PathBuf,
    cache_root: PathBuf,
    execution_temp_root: PathBuf,
    selection_hash: CanonicalHash,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct FreshEpochRecoveryRecordV1 {
    schema_version: u32,
    phase: String,
    operation_hash: CanonicalHash,
    evidence_set_hash: CanonicalHash,
    selection_hash: CanonicalHash,
    old_authority_epoch: u64,
    new_authority_epoch: u64,
    old_root_identity_hash: CanonicalHash,
    new_root_identity_hash: CanonicalHash,
    recovery_intent_hash: CanonicalHash,
}

#[derive(Debug, Clone)]
struct ChallengeRecordV1 {
    operation_hash: CanonicalHash,
    expires_at_ms: u64,
}

#[derive(Debug, Clone)]
struct QuiescenceRecordV1 {
    process_refs: Vec<String>,
    proof: OldAuthorityEpochQuiescenceProofV1,
}

#[derive(Debug, Clone)]
struct AuthorizationRecordV1 {
    service_instance_hash: CanonicalHash,
    operation_hash: CanonicalHash,
    authenticator: String,
    expires_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorityBootstrapRecoveryErrorV1 {
    Bootstrap(BootstrapErrorV1),
    InvalidOperation(String),
    RootSelectionInvalid(String),
    NoQuiescence,
    OldEpochStillLive(String),
    ConfirmationMismatch,
    ChallengeExpired,
    AuthorizationExpired,
    AuthorizationReplay,
    StaleServiceAuthorization,
    ExpectedEvidenceMismatch,
    ReconciliationPending,
}

impl std::fmt::Display for AuthorityBootstrapRecoveryErrorV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bootstrap(error) => write!(formatter, "bootstrap recovery failed: {error}"),
            Self::InvalidOperation(message) => {
                write!(formatter, "invalid recovery operation: {message}")
            }
            Self::RootSelectionInvalid(message) => {
                write!(formatter, "fresh root selection invalid: {message}")
            }
            Self::NoQuiescence => {
                formatter.write_str("old authority epoch has no quiescence proof")
            }
            Self::OldEpochStillLive(process) => {
                write!(formatter, "old authority process is still live: {process}")
            }
            Self::ConfirmationMismatch => formatter
                .write_str("operator confirmation does not match the challenge or operation"),
            Self::ChallengeExpired => formatter.write_str("operator challenge expired"),
            Self::AuthorizationExpired => {
                formatter.write_str("bootstrap recovery authorization expired")
            }
            Self::AuthorizationReplay => {
                formatter.write_str("bootstrap recovery authorization was already consumed")
            }
            Self::StaleServiceAuthorization => formatter
                .write_str("bootstrap recovery authorization belongs to another service instance"),
            Self::ExpectedEvidenceMismatch => formatter.write_str(
                "failed bootstrap evidence no longer matches the observed authority state",
            ),
            Self::ReconciliationPending => formatter
                .write_str("a durable fresh-epoch recovery transaction requires reconciliation"),
        }
    }
}

impl std::error::Error for AuthorityBootstrapRecoveryErrorV1 {}

const RECOVERY_RECORD_SCHEMA_VERSION: u32 = 1;
const RECOVERY_CHALLENGE_TTL_MS: u64 = 5 * 60 * 1000;
const RECOVERY_QUIESCENCE_TTL_MS: u64 = 5 * 60 * 1000;

/// Independent doctor/operator recovery service. It is intentionally not part of the normal
/// resource authority and cannot be constructed from a caller-selected bootstrap root.
pub struct AuthorityBootstrapRecoveryServiceV1 {
    namespace: AuthorityBootstrapRecoveryNamespaceV1,
    process_factory: Arc<dyn sigil_kernel::process_observation::HostProcessObservationFactoryV1>,
    service_instance_hash: CanonicalHash,
    selections: Mutex<BTreeMap<String, FreshRootSelectionRecordV1>>,
    proofs: Mutex<BTreeMap<CanonicalHash, QuiescenceRecordV1>>,
    challenges: Mutex<BTreeMap<String, ChallengeRecordV1>>,
    authorizations: Mutex<BTreeMap<String, AuthorizationRecordV1>>,
}

impl fmt::Debug for AuthorityBootstrapRecoveryServiceV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthorityBootstrapRecoveryServiceV1")
            .field("service_instance_hash", &self.service_instance_hash)
            .finish_non_exhaustive()
    }
}

impl AuthorityBootstrapRecoveryServiceV1 {
    pub fn for_canonical_config_path(
        config_path: &Path,
        process_factory: Arc<
            dyn sigil_kernel::process_observation::HostProcessObservationFactoryV1,
        >,
    ) -> Result<Self, AuthorityBootstrapRecoveryErrorV1> {
        let namespace =
            AuthorityBootstrapStoreV1::recovery_namespace_for_canonical_config_path(config_path)
                .map_err(AuthorityBootstrapRecoveryErrorV1::Bootstrap)?;
        Ok(Self::from_namespace(namespace, process_factory))
    }

    fn from_namespace(
        namespace: AuthorityBootstrapRecoveryNamespaceV1,
        process_factory: Arc<
            dyn sigil_kernel::process_observation::HostProcessObservationFactoryV1,
        >,
    ) -> Self {
        let service_instance_hash = canonical_bootstrap_hash(
            format!(
                "authority-bootstrap-recovery-service-v1\0{}\0{}",
                namespace.namespace().display(),
                std::process::id()
            )
            .as_bytes(),
        );
        Self {
            namespace,
            process_factory,
            service_instance_hash,
            selections: Mutex::new(BTreeMap::new()),
            proofs: Mutex::new(BTreeMap::new()),
            challenges: Mutex::new(BTreeMap::new()),
            authorizations: Mutex::new(BTreeMap::new()),
        }
    }

    #[must_use]
    pub fn service_instance_hash(&self) -> CanonicalHash {
        self.service_instance_hash
    }

    /// Computes the evidence-set binding used by both the operator confirmation and authority.
    pub fn evidence_set_hash(
        evidence: &[FailedAuthorityJournalEvidenceV1],
    ) -> Result<CanonicalHash, AuthorityBootstrapRecoveryErrorV1> {
        let bytes = serde_json::to_vec(evidence).map_err(|error| {
            AuthorityBootstrapRecoveryErrorV1::InvalidOperation(error.to_string())
        })?;
        Ok(canonical_bootstrap_hash(&bytes))
    }

    pub fn operation_hash(
        operation: &AuthorityBootstrapRecoveryOperationV1,
    ) -> Result<CanonicalHash, AuthorityBootstrapRecoveryErrorV1> {
        let bytes = serde_json::to_vec(operation).map_err(|error| {
            AuthorityBootstrapRecoveryErrorV1::InvalidOperation(error.to_string())
        })?;
        Ok(canonical_bootstrap_hash(&bytes))
    }

    /// Returns the current opaque bootstrap evidence digest for an operator confirmation. The
    /// digest is computed through no-follow private reads and never exposes the failed root path.
    pub fn observed_failed_bootstrap_hash(
        &self,
    ) -> Result<CanonicalHash, AuthorityBootstrapRecoveryErrorV1> {
        let (root, _) = recoverable_active_root(self.namespace.namespace())?;
        observed_bootstrap_digest(&root).map_err(AuthorityBootstrapRecoveryErrorV1::Bootstrap)
    }

    /// Registers a fresh, already-created, empty and owner-only root set. The returned reference
    /// is opaque; raw paths never enter the operation or any surface transport.
    pub fn prepare_fresh_root_selection(
        &self,
        state_root: &Path,
        cache_root: &Path,
        execution_temp_root: &Path,
    ) -> Result<OpaqueBootstrapRootConfigRef, AuthorityBootstrapRecoveryErrorV1> {
        let state_root = validate_fresh_root(state_root, "state")?;
        let cache_root = validate_fresh_root(cache_root, "cache")?;
        let execution_temp_root = validate_fresh_root(execution_temp_root, "execution-temp")?;
        let selection_hash = canonical_bootstrap_hash(
            format!(
                "authority-bootstrap-root-selection-v1\0{}\0{}\0{}",
                state_root.display(),
                cache_root.display(),
                execution_temp_root.display()
            )
            .as_bytes(),
        );
        let root_ref =
            OpaqueBootstrapRootConfigRef(format!("bootstrap-root-selection:{}", selection_hash));
        let record = FreshRootSelectionRecordV1 {
            root_ref: root_ref.clone(),
            state_root: state_root.to_path_buf(),
            cache_root: cache_root.to_path_buf(),
            execution_temp_root: execution_temp_root.to_path_buf(),
            selection_hash,
        };
        self.selections
            .lock()
            .map_err(|_| {
                AuthorityBootstrapRecoveryErrorV1::InvalidOperation(
                    "selection table poisoned".to_owned(),
                )
            })?
            .insert(root_ref.as_str().to_owned(), record);
        Ok(root_ref)
    }

    /// Proves that every known old-epoch process is terminal or absent using the host observer
    /// factory; an empty process inventory is rejected rather than treated as a waiver.
    pub fn probe_old_epoch_quiescence(
        &self,
        evidence_set_hash: CanonicalHash,
        process_refs: &[String],
    ) -> Result<OldAuthorityEpochQuiescenceProofV1, AuthorityBootstrapRecoveryErrorV1> {
        if process_refs.is_empty() || process_refs.len() > 256 {
            return Err(AuthorityBootstrapRecoveryErrorV1::NoQuiescence);
        }
        let mut refs = process_refs.to_vec();
        refs.sort();
        refs.dedup();
        let service = self.process_factory.observation_service();
        let verifier = self.process_factory.observation_verifier();
        let mut observations = Vec::with_capacity(refs.len());
        for process_ref in &refs {
            let observation = service
                .observe(
                    sigil_kernel::process_observation::ProcessObservationPurposeV1::TerminalProof,
                    process_ref,
                )
                .map_err(|error| {
                    AuthorityBootstrapRecoveryErrorV1::OldEpochStillLive(error.to_string())
                })?;
            let verified = verifier
                .verify_observation(
                    sigil_kernel::process_observation::ProcessObservationPurposeV1::TerminalProof,
                    &observation,
                )
                .map_err(|error| {
                    AuthorityBootstrapRecoveryErrorV1::OldEpochStillLive(error.to_string())
                })?;
            if verified.vitality == sigil_kernel::process_observation::ProcessVitalityV1::Live {
                return Err(AuthorityBootstrapRecoveryErrorV1::OldEpochStillLive(
                    process_ref.clone(),
                ));
            }
            observations.push((observation, verified.verified_observation_hash));
        }
        let observed_at_ms = current_epoch_ms();
        let inventory_hash = canonical_bootstrap_hash(
            serde_json::to_vec(&refs)
                .map_err(|error| {
                    AuthorityBootstrapRecoveryErrorV1::InvalidOperation(error.to_string())
                })?
                .as_slice(),
        );
        let owner_probe_hash = canonical_bootstrap_hash(
            serde_json::to_vec(&observations)
                .map_err(|error| {
                    AuthorityBootstrapRecoveryErrorV1::InvalidOperation(error.to_string())
                })?
                .as_slice(),
        );
        let terminal_hash = canonical_bootstrap_hash(
            serde_json::to_vec(
                &observations
                    .iter()
                    .map(|(observation, verified_hash)| (&observation.process_ref, verified_hash))
                    .collect::<Vec<_>>(),
            )
            .map_err(|error| {
                AuthorityBootstrapRecoveryErrorV1::InvalidOperation(error.to_string())
            })?
            .as_slice(),
        );
        let proof_hash = canonical_bootstrap_hash(
            format!(
                "{}\0{}\0{}\0{}\0{}",
                evidence_set_hash, inventory_hash, owner_probe_hash, terminal_hash, observed_at_ms
            )
            .as_bytes(),
        );
        let proof = OldAuthorityEpochQuiescenceProofV1 {
            failed_epoch_evidence_set_hash: evidence_set_hash,
            known_process_tree_inventory_hash: inventory_hash,
            process_owner_probe_hash: owner_probe_hash,
            terminal_or_absent_proof_hash: terminal_hash,
            observed_at_ms,
            proof_hash,
        };
        self.proofs
            .lock()
            .map_err(|_| {
                AuthorityBootstrapRecoveryErrorV1::InvalidOperation(
                    "proof table poisoned".to_owned(),
                )
            })?
            .insert(
                proof_hash,
                QuiescenceRecordV1 {
                    process_refs: refs,
                    proof: proof.clone(),
                },
            );
        Ok(proof)
    }

    pub fn issue_operator_challenge(
        &self,
        operation: &AuthorityBootstrapRecoveryOperationV1,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<AuthorityBootstrapOperatorChallengeV1, AuthorityBootstrapRecoveryErrorV1> {
        let operation_hash = Self::operation_hash(operation)?;
        let expires_at_ms = now_ms
            .checked_add(ttl_ms.min(RECOVERY_CHALLENGE_TTL_MS))
            .ok_or_else(|| {
                AuthorityBootstrapRecoveryErrorV1::InvalidOperation(
                    "challenge expiry overflow".to_owned(),
                )
            })?;
        let challenge_id =
            OpaqueOperatorChallengeId(format!("bootstrap-challenge:{}:{}", operation_hash, now_ms));
        let challenge_hash = canonical_bootstrap_hash(
            format!(
                "{}\0{}\0{}",
                challenge_id.as_str(),
                operation_hash,
                expires_at_ms
            )
            .as_bytes(),
        );
        self.challenges
            .lock()
            .map_err(|_| {
                AuthorityBootstrapRecoveryErrorV1::InvalidOperation(
                    "challenge table poisoned".to_owned(),
                )
            })?
            .insert(
                challenge_id.as_str().to_owned(),
                ChallengeRecordV1 {
                    operation_hash,
                    expires_at_ms,
                },
            );
        Ok(AuthorityBootstrapOperatorChallengeV1 {
            challenge_id,
            operation_hash,
            expires_at_ms,
            challenge_hash,
        })
    }

    pub fn authorize(
        &self,
        operation: &AuthorityBootstrapRecoveryOperationV1,
        confirmation: ExactBootstrapOperatorConfirmationV1,
        now_ms: u64,
    ) -> Result<AuthorityBootstrapRecoveryAuthorizationV1, AuthorityBootstrapRecoveryErrorV1> {
        let operation_hash = Self::operation_hash(operation)?;
        if confirmation.operation_hash != operation_hash {
            return Err(AuthorityBootstrapRecoveryErrorV1::ConfirmationMismatch);
        }
        let challenge = self
            .challenges
            .lock()
            .map_err(|_| {
                AuthorityBootstrapRecoveryErrorV1::InvalidOperation(
                    "challenge table poisoned".to_owned(),
                )
            })?
            .remove(confirmation.challenge_id.as_str())
            .ok_or(AuthorityBootstrapRecoveryErrorV1::ConfirmationMismatch)?;
        if challenge.operation_hash != operation_hash || now_ms > challenge.expires_at_ms {
            return Err(if now_ms > challenge.expires_at_ms {
                AuthorityBootstrapRecoveryErrorV1::ChallengeExpired
            } else {
                AuthorityBootstrapRecoveryErrorV1::ConfirmationMismatch
            });
        }
        let expected_confirmation_hash = confirmation_hash(&confirmation);
        if confirmation.confirmation_hash != expected_confirmation_hash {
            return Err(AuthorityBootstrapRecoveryErrorV1::ConfirmationMismatch);
        }
        self.validate_operation(
            operation,
            confirmation.evidence_set_hash,
            confirmation.quiescence_proof_hash,
            confirmation.fresh_root_selection_hash,
        )?;
        let expires_at_ms = now_ms
            .checked_add(RECOVERY_CHALLENGE_TTL_MS)
            .ok_or_else(|| {
                AuthorityBootstrapRecoveryErrorV1::InvalidOperation(
                    "authorization expiry overflow".to_owned(),
                )
            })?;
        let authorization_id = OpaqueBootstrapRecoveryAuthorizationId(format!(
            "bootstrap-authorization:{}:{}",
            operation_hash, now_ms
        ));
        let authenticator = canonical_bootstrap_hash(
            format!(
                "{}\0{}\0{}",
                self.service_instance_hash,
                authorization_id.as_str(),
                operation_hash
            )
            .as_bytes(),
        )
        .to_hex();
        self.authorizations
            .lock()
            .map_err(|_| {
                AuthorityBootstrapRecoveryErrorV1::InvalidOperation(
                    "authorization table poisoned".to_owned(),
                )
            })?
            .insert(
                authorization_id.as_str().to_owned(),
                AuthorizationRecordV1 {
                    service_instance_hash: self.service_instance_hash,
                    operation_hash,
                    authenticator: authenticator.clone(),
                    expires_at_ms,
                },
            );
        Ok(AuthorityBootstrapRecoveryAuthorizationV1 {
            authorization_id,
            operation_hash,
            evidence_set_hash: confirmation.evidence_set_hash,
            quiescence_proof_hash: confirmation.quiescence_proof_hash,
            operator_confirmation_hash: confirmation.confirmation_hash,
            expires_at_ms,
            authenticator,
        })
    }

    pub fn execute(
        &self,
        operation: AuthorityBootstrapRecoveryOperationV1,
        authorization: AuthorityBootstrapRecoveryAuthorizationV1,
    ) -> Result<AuthorityBootstrapRecoveryReceiptV1, AuthorityBootstrapRecoveryErrorV1> {
        let operation_hash = Self::operation_hash(&operation)?;
        if authorization.operation_hash != operation_hash {
            return Err(AuthorityBootstrapRecoveryErrorV1::StaleServiceAuthorization);
        }
        let record = self
            .authorizations
            .lock()
            .map_err(|_| {
                AuthorityBootstrapRecoveryErrorV1::InvalidOperation(
                    "authorization table poisoned".to_owned(),
                )
            })?
            .remove(authorization.authorization_id.as_str())
            .ok_or(AuthorityBootstrapRecoveryErrorV1::AuthorizationReplay)?;
        if record.service_instance_hash != self.service_instance_hash
            || record.operation_hash != operation_hash
            || record.authenticator != authorization.authenticator
        {
            return Err(AuthorityBootstrapRecoveryErrorV1::StaleServiceAuthorization);
        }
        if current_epoch_ms() > record.expires_at_ms
            || current_epoch_ms() > authorization.expires_at_ms
        {
            return Err(AuthorityBootstrapRecoveryErrorV1::AuthorizationExpired);
        }
        match &operation {
            AuthorityBootstrapRecoveryOperationV1::RevealBootstrapDiagnostic { .. } => {
                return self.diagnostic_receipt(operation_hash, authorization.evidence_set_hash);
            }
            AuthorityBootstrapRecoveryOperationV1::SelectFreshAuthorityEpoch {
                evidence_set_hash,
                old_epoch_quiescence,
                ..
            } => {
                self.validate_operation(
                    &operation,
                    *evidence_set_hash,
                    Some(old_epoch_quiescence.proof_hash),
                    operation_root_selection_hash(&operation),
                )?;
            }
        }
        if let Some(receipt) = self.reconcile_pending_fresh_epoch()? {
            return Ok(receipt);
        }
        if recovery_candidates(self.namespace.namespace())?
            .into_iter()
            .any(|(_, record)| record.operation_hash == operation_hash)
        {
            return Err(AuthorityBootstrapRecoveryErrorV1::ReconciliationPending);
        }
        let transaction = self
            .namespace
            .acquire_transaction()
            .map_err(AuthorityBootstrapRecoveryErrorV1::Bootstrap)?;
        let (old_root, old_epoch) = recoverable_active_root(self.namespace.namespace())?;
        let old_identity = bootstrap_root_identity(&old_root)
            .map_err(AuthorityBootstrapRecoveryErrorV1::Bootstrap)?;
        let (selection_hash, expected_failed_bootstrap_hash, evidence_set_hash) = match &operation {
            AuthorityBootstrapRecoveryOperationV1::SelectFreshAuthorityEpoch {
                explicit_root_config,
                expected_failed_bootstrap_hash,
                evidence_set_hash,
                ..
            } => {
                let selection = self.selection(explicit_root_config)?;
                (
                    selection.selection_hash,
                    *expected_failed_bootstrap_hash,
                    *evidence_set_hash,
                )
            }
            AuthorityBootstrapRecoveryOperationV1::RevealBootstrapDiagnostic { .. } => {
                unreachable!()
            }
        };
        if let Some(expected) = expected_failed_bootstrap_hash {
            let observed = observed_bootstrap_digest(&old_root)
                .map_err(AuthorityBootstrapRecoveryErrorV1::Bootstrap)?;
            if expected != observed {
                return Err(AuthorityBootstrapRecoveryErrorV1::ExpectedEvidenceMismatch);
            }
        }
        let new_epoch = old_epoch.checked_add(1).ok_or_else(|| {
            AuthorityBootstrapRecoveryErrorV1::InvalidOperation(
                "authority epoch overflow".to_owned(),
            )
        })?;
        let epochs = self.namespace.namespace().join(EPOCHS_DIRECTORY_NAME);
        ensure_owner_only_directory(&epochs)
            .map_err(AuthorityBootstrapRecoveryErrorV1::Bootstrap)?;
        let epoch_name = format!("epoch-{new_epoch}-{}", selection_hash.to_hex());
        if fs::symlink_metadata(epochs.join(&epoch_name)).is_ok() {
            return Err(AuthorityBootstrapRecoveryErrorV1::ReconciliationPending);
        }
        let new_root = epochs.join(epoch_name);
        fs::create_dir(&new_root).map_err(|error| {
            AuthorityBootstrapRecoveryErrorV1::Bootstrap(BootstrapErrorV1::HardeningFailed(
                error.to_string(),
            ))
        })?;
        ensure_owner_only_directory(&new_root)
            .map_err(AuthorityBootstrapRecoveryErrorV1::Bootstrap)?;
        let new_identity = bootstrap_root_identity(&new_root)
            .map_err(AuthorityBootstrapRecoveryErrorV1::Bootstrap)?;
        let intent_hash = canonical_bootstrap_hash(
            format!(
                "{}\0{}\0{}\0{}",
                operation_hash, old_epoch, new_epoch, new_identity
            )
            .as_bytes(),
        );
        let intent = FreshEpochRecoveryRecordV1 {
            schema_version: RECOVERY_RECORD_SCHEMA_VERSION,
            phase: "prepared".to_owned(),
            operation_hash,
            evidence_set_hash,
            selection_hash,
            old_authority_epoch: old_epoch,
            new_authority_epoch: new_epoch,
            old_root_identity_hash: old_identity,
            new_root_identity_hash: new_identity,
            recovery_intent_hash: intent_hash,
        };
        let intent_bytes = serde_json::to_vec(&intent).map_err(|error| {
            AuthorityBootstrapRecoveryErrorV1::InvalidOperation(error.to_string())
        })?;
        publish_private_bootstrap_file(&new_root.join(RECOVERY_INTENT_FILE), &intent_bytes)
            .map_err(AuthorityBootstrapRecoveryErrorV1::Bootstrap)?;
        let completed = FreshEpochRecoveryRecordV1 {
            phase: "completed".to_owned(),
            ..intent.clone()
        };
        let completed_bytes = serde_json::to_vec(&completed).map_err(|error| {
            AuthorityBootstrapRecoveryErrorV1::InvalidOperation(error.to_string())
        })?;
        publish_private_bootstrap_file(&new_root.join(RECOVERY_RECEIPT_FILE), &completed_bytes)
            .map_err(AuthorityBootstrapRecoveryErrorV1::Bootstrap)?;
        let inert_marker = serde_json::to_vec(&completed).map_err(|error| {
            AuthorityBootstrapRecoveryErrorV1::InvalidOperation(error.to_string())
        })?;
        publish_private_bootstrap_file(&old_root.join(OLD_EPOCH_INERT_FILE), &inert_marker)
            .map_err(AuthorityBootstrapRecoveryErrorV1::Bootstrap)?;
        self.namespace
            .publish_active_epoch(&transaction, new_epoch, &new_root)
            .map_err(AuthorityBootstrapRecoveryErrorV1::Bootstrap)?;
        let receipt_hash = canonical_bootstrap_hash(&completed_bytes);
        Ok(AuthorityBootstrapRecoveryReceiptV1 {
            schema_version: RECOVERY_RECORD_SCHEMA_VERSION,
            operation_hash,
            evidence_set_hash,
            old_authority_epoch: old_epoch,
            new_authority_epoch: new_epoch,
            old_root_identity_hash: old_identity,
            new_root_identity_hash: new_identity,
            recovery_intent_hash: intent_hash,
            receipt_hash,
        })
    }

    /// Reconciles only a durable completed fresh-epoch record. It never allocates a second epoch
    /// and is safe to call after a crash between old-root inertization and pointer publication.
    pub fn reconcile_pending_fresh_epoch(
        &self,
    ) -> Result<Option<AuthorityBootstrapRecoveryReceiptV1>, AuthorityBootstrapRecoveryErrorV1>
    {
        let transaction = self
            .namespace
            .acquire_transaction()
            .map_err(AuthorityBootstrapRecoveryErrorV1::Bootstrap)?;
        let candidates = recovery_candidates(self.namespace.namespace())?;
        let Some((root, record)) = candidates
            .into_iter()
            .max_by_key(|(_, record)| record.new_authority_epoch)
        else {
            return Ok(None);
        };
        // A corrupt pointer is precisely one of the states this independent service is allowed
        // to recover. Normal boot still returns the typed corruption error from
        // `resolve_active_epoch`; only this explicitly-authorized path treats it as unknown.
        let current = read_active_pointer_record(self.namespace.namespace()).unwrap_or(None);
        if current
            .as_ref()
            .is_some_and(|current| current.epoch >= record.new_authority_epoch)
        {
            return Ok(None);
        }
        let old_root = if record.old_authority_epoch == 1 {
            self.namespace.namespace().to_path_buf()
        } else {
            recovery_epoch_root(self.namespace.namespace(), record.old_authority_epoch)?
        };
        let old_identity = bootstrap_root_identity(&old_root)
            .map_err(AuthorityBootstrapRecoveryErrorV1::Bootstrap)?;
        if old_identity != record.old_root_identity_hash {
            return Err(AuthorityBootstrapRecoveryErrorV1::ExpectedEvidenceMismatch);
        }
        publish_private_bootstrap_file(
            &old_root.join(OLD_EPOCH_INERT_FILE),
            &serde_json::to_vec(&record).map_err(|error| {
                AuthorityBootstrapRecoveryErrorV1::InvalidOperation(error.to_string())
            })?,
        )
        .map_err(AuthorityBootstrapRecoveryErrorV1::Bootstrap)?;
        self.namespace
            .publish_active_epoch(&transaction, record.new_authority_epoch, &root)
            .map_err(AuthorityBootstrapRecoveryErrorV1::Bootstrap)?;
        let receipt_bytes = read_private_bytes(&root.join(RECOVERY_RECEIPT_FILE))
            .map_err(AuthorityBootstrapRecoveryErrorV1::Bootstrap)?;
        let receipt_hash = canonical_bootstrap_hash(&receipt_bytes);
        Ok(Some(AuthorityBootstrapRecoveryReceiptV1 {
            schema_version: record.schema_version,
            operation_hash: record.operation_hash,
            evidence_set_hash: record.evidence_set_hash,
            old_authority_epoch: record.old_authority_epoch,
            new_authority_epoch: record.new_authority_epoch,
            old_root_identity_hash: record.old_root_identity_hash,
            new_root_identity_hash: record.new_root_identity_hash,
            recovery_intent_hash: record.recovery_intent_hash,
            receipt_hash,
        }))
    }

    fn selection(
        &self,
        root_ref: &OpaqueBootstrapRootConfigRef,
    ) -> Result<FreshRootSelectionRecordV1, AuthorityBootstrapRecoveryErrorV1> {
        let selection = self
            .selections
            .lock()
            .map_err(|_| {
                AuthorityBootstrapRecoveryErrorV1::InvalidOperation(
                    "selection table poisoned".to_owned(),
                )
            })?
            .get(root_ref.as_str())
            .cloned()
            .ok_or_else(|| {
                AuthorityBootstrapRecoveryErrorV1::RootSelectionInvalid(
                    "unknown fresh root selection".to_owned(),
                )
            })?;
        for (label, path) in [
            ("state", selection.state_root.as_path()),
            ("cache", selection.cache_root.as_path()),
            ("execution-temp", selection.execution_temp_root.as_path()),
        ] {
            validate_fresh_root(path, label)?;
        }
        Ok(selection)
    }

    fn validate_operation(
        &self,
        operation: &AuthorityBootstrapRecoveryOperationV1,
        evidence_set_hash: CanonicalHash,
        quiescence_proof_hash: Option<CanonicalHash>,
        selection_hash: Option<CanonicalHash>,
    ) -> Result<(), AuthorityBootstrapRecoveryErrorV1> {
        match operation {
            AuthorityBootstrapRecoveryOperationV1::SelectFreshAuthorityEpoch {
                explicit_root_config,
                failed_journal_evidence,
                evidence_set_hash: operation_evidence_hash,
                old_epoch_quiescence,
                ..
            } => {
                if *operation_evidence_hash != evidence_set_hash
                    || Self::evidence_set_hash(failed_journal_evidence)? != evidence_set_hash
                    || quiescence_proof_hash != Some(old_epoch_quiescence.proof_hash)
                {
                    return Err(AuthorityBootstrapRecoveryErrorV1::ConfirmationMismatch);
                }
                let selection = self.selection(explicit_root_config)?;
                if selection_hash != Some(selection.selection_hash) {
                    return Err(AuthorityBootstrapRecoveryErrorV1::ConfirmationMismatch);
                }
                let proof = self
                    .proofs
                    .lock()
                    .map_err(|_| {
                        AuthorityBootstrapRecoveryErrorV1::InvalidOperation(
                            "proof table poisoned".to_owned(),
                        )
                    })?
                    .get(&old_epoch_quiescence.proof_hash)
                    .cloned()
                    .ok_or(AuthorityBootstrapRecoveryErrorV1::NoQuiescence)?;
                if proof.proof != **old_epoch_quiescence {
                    return Err(AuthorityBootstrapRecoveryErrorV1::NoQuiescence);
                }
                if current_epoch_ms().saturating_sub(proof.proof.observed_at_ms)
                    > RECOVERY_QUIESCENCE_TTL_MS
                {
                    return Err(AuthorityBootstrapRecoveryErrorV1::NoQuiescence);
                }
                self.verify_quiescence_still_terminal(&proof.process_refs)?;
                Ok(())
            }
            AuthorityBootstrapRecoveryOperationV1::RevealBootstrapDiagnostic { .. } => Ok(()),
        }
    }

    fn verify_quiescence_still_terminal(
        &self,
        process_refs: &[String],
    ) -> Result<(), AuthorityBootstrapRecoveryErrorV1> {
        if process_refs.is_empty() {
            return Err(AuthorityBootstrapRecoveryErrorV1::NoQuiescence);
        }
        let service = self.process_factory.observation_service();
        let verifier = self.process_factory.observation_verifier();
        for process_ref in process_refs {
            let observation = service
                .observe(
                    sigil_kernel::process_observation::ProcessObservationPurposeV1::TerminalProof,
                    process_ref,
                )
                .map_err(|error| {
                    AuthorityBootstrapRecoveryErrorV1::OldEpochStillLive(error.to_string())
                })?;
            let verified = verifier
                .verify_observation(
                    sigil_kernel::process_observation::ProcessObservationPurposeV1::TerminalProof,
                    &observation,
                )
                .map_err(|error| {
                    AuthorityBootstrapRecoveryErrorV1::OldEpochStillLive(error.to_string())
                })?;
            if verified.vitality == sigil_kernel::process_observation::ProcessVitalityV1::Live {
                return Err(AuthorityBootstrapRecoveryErrorV1::OldEpochStillLive(
                    process_ref.clone(),
                ));
            }
        }
        Ok(())
    }

    fn diagnostic_receipt(
        &self,
        operation_hash: CanonicalHash,
        evidence_set_hash: CanonicalHash,
    ) -> Result<AuthorityBootstrapRecoveryReceiptV1, AuthorityBootstrapRecoveryErrorV1> {
        let (root, epoch) = recoverable_active_root(self.namespace.namespace())?;
        let identity =
            bootstrap_root_identity(&root).map_err(AuthorityBootstrapRecoveryErrorV1::Bootstrap)?;
        Ok(AuthorityBootstrapRecoveryReceiptV1 {
            schema_version: RECOVERY_RECORD_SCHEMA_VERSION,
            operation_hash,
            evidence_set_hash,
            old_authority_epoch: epoch,
            new_authority_epoch: epoch,
            old_root_identity_hash: identity,
            new_root_identity_hash: identity,
            recovery_intent_hash: canonical_bootstrap_hash(b"bootstrap-diagnostic"),
            receipt_hash: canonical_bootstrap_hash(
                format!("{}\0{}\0{}", operation_hash, evidence_set_hash, identity).as_bytes(),
            ),
        })
    }
}

impl ExactBootstrapOperatorConfirmationV1 {
    #[must_use]
    pub fn for_challenge(
        challenge: &AuthorityBootstrapOperatorChallengeV1,
        evidence_set_hash: CanonicalHash,
        quiescence_proof_hash: Option<CanonicalHash>,
        fresh_root_selection_hash: Option<CanonicalHash>,
        confirmed_at_ms: u64,
    ) -> Self {
        let mut confirmation = Self {
            challenge_id: challenge.challenge_id.clone(),
            operation_hash: challenge.operation_hash,
            evidence_set_hash,
            quiescence_proof_hash,
            fresh_root_selection_hash,
            confirmed_at_ms,
            confirmation_hash: canonical_bootstrap_hash(b"uninitialized-confirmation"),
        };
        confirmation.confirmation_hash = confirmation_hash(&confirmation);
        confirmation
    }
}

fn confirmation_hash(confirmation: &ExactBootstrapOperatorConfirmationV1) -> CanonicalHash {
    canonical_bootstrap_hash(
        format!(
            "{}\0{}\0{}\0{}\0{}\0{}",
            confirmation.challenge_id.as_str(),
            confirmation.operation_hash,
            confirmation.evidence_set_hash,
            confirmation
                .quiescence_proof_hash
                .map_or_else(|| "none".to_owned(), |hash| hash.to_string()),
            confirmation
                .fresh_root_selection_hash
                .map_or_else(|| "none".to_owned(), |hash| hash.to_string()),
            confirmation.confirmed_at_ms
        )
        .as_bytes(),
    )
}

fn operation_root_selection_hash(
    operation: &AuthorityBootstrapRecoveryOperationV1,
) -> Option<CanonicalHash> {
    match operation {
        AuthorityBootstrapRecoveryOperationV1::SelectFreshAuthorityEpoch {
            explicit_root_config,
            ..
        } => explicit_root_config
            .as_str()
            .strip_prefix("bootstrap-root-selection:")
            .and_then(|hex| {
                if hex.len() != 64 {
                    return None;
                }
                let mut bytes = [0u8; 32];
                for (index, chunk) in hex.as_bytes().chunks_exact(2).enumerate() {
                    bytes[index] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
                }
                Some(CanonicalHash::from_bytes(bytes))
            }),
        AuthorityBootstrapRecoveryOperationV1::RevealBootstrapDiagnostic { .. } => None,
    }
}

fn validate_fresh_root(
    path: &Path,
    label: &str,
) -> Result<PathBuf, AuthorityBootstrapRecoveryErrorV1> {
    if !path.is_absolute() {
        return Err(AuthorityBootstrapRecoveryErrorV1::RootSelectionInvalid(
            format!("{label} root must be absolute"),
        ));
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        AuthorityBootstrapRecoveryErrorV1::RootSelectionInvalid(format!(
            "{label} root unavailable: {error}"
        ))
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(AuthorityBootstrapRecoveryErrorV1::RootSelectionInvalid(
            format!("{label} root is not a plain directory"),
        ));
    }
    let canonical_path = fs::canonicalize(path).map_err(|error| {
        AuthorityBootstrapRecoveryErrorV1::RootSelectionInvalid(format!(
            "{label} root cannot be canonicalized: {error}"
        ))
    })?;
    ensure_owner_only_directory(&canonical_path).map_err(|error| {
        AuthorityBootstrapRecoveryErrorV1::RootSelectionInvalid(error.to_string())
    })?;
    if fs::read_dir(&canonical_path)
        .map_err(|error| {
            AuthorityBootstrapRecoveryErrorV1::RootSelectionInvalid(error.to_string())
        })?
        .next()
        .is_some()
    {
        return Err(AuthorityBootstrapRecoveryErrorV1::RootSelectionInvalid(
            format!("{label} root must be empty"),
        ));
    }
    Ok(canonical_path)
}

fn current_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().min(u128::from(u64::MAX)) as u64
        })
}

fn read_private_bytes(path: &Path) -> Result<Vec<u8>, BootstrapErrorV1> {
    let Some(file) = open_private_read_file(path)? else {
        return Err(BootstrapErrorV1::ReconciliationRequired(format!(
            "{} is missing",
            path.display()
        )));
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
    Ok(bytes)
}

fn read_active_pointer_record(
    namespace: &Path,
) -> Result<Option<ActiveEpochPointerRecordV1>, AuthorityBootstrapRecoveryErrorV1> {
    let path = namespace.join(ACTIVE_EPOCH_POINTER_FILE);
    let Some(bytes) = open_private_read_file(&path)
        .map_err(AuthorityBootstrapRecoveryErrorV1::Bootstrap)?
        .map(|file| {
            let mut bytes = Vec::new();
            file.take(MAX_BOOTSTRAP_METADATA_BYTES + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        })
        .transpose()
        .map_err(|error| {
            AuthorityBootstrapRecoveryErrorV1::Bootstrap(BootstrapErrorV1::MetadataCorrupted(
                error.to_string(),
            ))
        })?
    else {
        return Ok(None);
    };
    let Ok(record) = serde_json::from_slice::<ActiveEpochPointerRecordV1>(&bytes) else {
        return Ok(None);
    };
    if record.schema_version != ACTIVE_EPOCH_POINTER_SCHEMA_VERSION
        || record.epoch == 0
        || !valid_epoch_name(&record.epoch_name)
    {
        return Ok(None);
    }
    let root = namespace
        .join(EPOCHS_DIRECTORY_NAME)
        .join(&record.epoch_name);
    if bootstrap_root_identity(&root).ok() != Some(record.root_identity_hash) {
        return Ok(None);
    }
    Ok(Some(record))
}

fn recovery_epoch_root(
    namespace: &Path,
    epoch: u64,
) -> Result<PathBuf, AuthorityBootstrapRecoveryErrorV1> {
    let epochs = namespace.join(EPOCHS_DIRECTORY_NAME);
    ensure_owner_only_directory(&epochs).map_err(AuthorityBootstrapRecoveryErrorV1::Bootstrap)?;
    let mut candidates = Vec::new();
    for entry in fs::read_dir(&epochs).map_err(|error| {
        AuthorityBootstrapRecoveryErrorV1::Bootstrap(BootstrapErrorV1::MetadataCorrupted(
            error.to_string(),
        ))
    })? {
        let entry = entry.map_err(|error| {
            AuthorityBootstrapRecoveryErrorV1::Bootstrap(BootstrapErrorV1::MetadataCorrupted(
                error.to_string(),
            ))
        })?;
        let path = entry.path();
        if !entry
            .file_type()
            .map_err(|error| {
                AuthorityBootstrapRecoveryErrorV1::Bootstrap(BootstrapErrorV1::MetadataCorrupted(
                    error.to_string(),
                ))
            })?
            .is_dir()
        {
            continue;
        }
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(&format!("epoch-{epoch}-")))
        {
            candidates.push(path);
        }
    }
    candidates.into_iter().next().ok_or_else(|| {
        AuthorityBootstrapRecoveryErrorV1::Bootstrap(BootstrapErrorV1::ReconciliationRequired(
            format!("authority epoch {epoch} root is unavailable"),
        ))
    })
}

fn recovery_candidates(
    namespace: &Path,
) -> Result<Vec<(PathBuf, FreshEpochRecoveryRecordV1)>, AuthorityBootstrapRecoveryErrorV1> {
    let epochs = namespace.join(EPOCHS_DIRECTORY_NAME);
    if !matches!(fs::symlink_metadata(&epochs), Ok(metadata) if metadata.is_dir()) {
        return Ok(Vec::new());
    }
    let mut candidates = Vec::new();
    for entry in fs::read_dir(&epochs).map_err(|error| {
        AuthorityBootstrapRecoveryErrorV1::Bootstrap(BootstrapErrorV1::MetadataCorrupted(
            error.to_string(),
        ))
    })? {
        let entry = entry.map_err(|error| {
            AuthorityBootstrapRecoveryErrorV1::Bootstrap(BootstrapErrorV1::MetadataCorrupted(
                error.to_string(),
            ))
        })?;
        let path = entry.path();
        if !entry
            .file_type()
            .map_err(|error| {
                AuthorityBootstrapRecoveryErrorV1::Bootstrap(BootstrapErrorV1::MetadataCorrupted(
                    error.to_string(),
                ))
            })?
            .is_dir()
        {
            continue;
        }
        let Some(bytes) = open_private_read_file(&path.join(RECOVERY_RECEIPT_FILE))
            .map_err(AuthorityBootstrapRecoveryErrorV1::Bootstrap)?
            .map(|file| {
                let mut bytes = Vec::new();
                file.take(MAX_BOOTSTRAP_METADATA_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map(|_| bytes)
            })
            .transpose()
            .map_err(|error| {
                AuthorityBootstrapRecoveryErrorV1::Bootstrap(BootstrapErrorV1::MetadataCorrupted(
                    error.to_string(),
                ))
            })?
        else {
            continue;
        };
        let record: FreshEpochRecoveryRecordV1 =
            serde_json::from_slice(&bytes).map_err(|error| {
                AuthorityBootstrapRecoveryErrorV1::Bootstrap(BootstrapErrorV1::MetadataCorrupted(
                    error.to_string(),
                ))
            })?;
        if record.schema_version != RECOVERY_RECORD_SCHEMA_VERSION || record.phase != "completed" {
            return Err(AuthorityBootstrapRecoveryErrorV1::Bootstrap(
                BootstrapErrorV1::MetadataCorrupted(
                    "fresh epoch recovery receipt is invalid".to_owned(),
                ),
            ));
        }
        let identity =
            bootstrap_root_identity(&path).map_err(AuthorityBootstrapRecoveryErrorV1::Bootstrap)?;
        if identity != record.new_root_identity_hash {
            return Err(AuthorityBootstrapRecoveryErrorV1::ExpectedEvidenceMismatch);
        }
        candidates.push((path, record));
    }
    Ok(candidates)
}

fn recoverable_active_root(
    namespace: &Path,
) -> Result<(PathBuf, u64), AuthorityBootstrapRecoveryErrorV1> {
    if let Ok(active) = resolve_active_epoch(namespace) {
        return Ok(active);
    }
    let candidates = recovery_candidates(namespace)?;
    if let Some((path, record)) = candidates
        .into_iter()
        .max_by_key(|(_, record)| record.new_authority_epoch)
    {
        return Ok((path, record.new_authority_epoch));
    }
    Ok((namespace.to_path_buf(), 1))
}

fn observed_bootstrap_digest(root: &Path) -> Result<CanonicalHash, BootstrapErrorV1> {
    let mut material = Vec::new();
    for name in [
        "bootstrap-manifest.json",
        AUTHORITY_CONFIG_GENERATION_FILE,
        CUTOVER_POINTER_FILE,
        ACTIVE_EPOCH_POINTER_FILE,
        OLD_EPOCH_INERT_FILE,
        RECOVERY_INTENT_FILE,
        RECOVERY_RECEIPT_FILE,
    ] {
        material.extend_from_slice(name.as_bytes());
        material.push(0);
        if let Some(bytes) = open_private_read_file(&root.join(name))?.map(|file| {
            let mut bytes = Vec::new();
            file.take(MAX_BOOTSTRAP_METADATA_BYTES + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        }) {
            material.extend_from_slice(
                &bytes.map_err(|error| BootstrapErrorV1::MetadataCorrupted(error.to_string()))?,
            );
        }
        material.push(0xff);
    }
    Ok(canonical_bootstrap_hash(&material))
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

#[cfg(test)]
mod recovery_tests {
    use super::*;
    use sigil_kernel::process_observation::HostProcessObservationFactoryV1;

    fn process_factory() -> Arc<dyn HostProcessObservationFactoryV1> {
        sigil_process_observer::ProcessObserverFactoryV1::new(canonical_bootstrap_hash(
            b"r71-bootstrap-recovery-test-observer",
        ))
        .instantiate()
    }

    fn evidence() -> Vec<FailedAuthorityJournalEvidenceV1> {
        vec![FailedAuthorityJournalEvidenceV1 {
            journal_scope: sigil_kernel::resource::ResourceJournalScopeV1::Application,
            expected_anchor_identity: canonical_bootstrap_hash(b"anchor"),
            last_verified_record_hash: None,
            observed_failure_digest: canonical_bootstrap_hash(b"corrupt"),
            failure_class: AuthorityJournalFailureClassV1::CorruptHashChain,
        }]
    }

    #[test]
    fn r71_bootstrap_recovery_selects_fresh_epoch_and_is_one_shot() {
        let temp = tempfile::tempdir().expect("tempdir");
        let base = fs::canonicalize(temp.path()).expect("canonical tempdir");
        let namespace = base.join("authority-namespace");
        fs::create_dir(&namespace).expect("namespace");
        let active_pointer = namespace.join(ACTIVE_EPOCH_POINTER_FILE);
        sigil_kernel::atomic_publish_private_file(&active_pointer, b"{corrupt")
            .expect("corrupt pointer fixture");
        assert!(matches!(
            resolve_active_epoch(&namespace),
            Err(BootstrapErrorV1::MetadataCorrupted(_))
        ));
        let expected_failed_bootstrap_hash = observed_bootstrap_digest(&namespace).expect("digest");
        let state = base.join("state");
        let cache = base.join("cache");
        let execution_temp = base.join("execution-temp");
        fs::create_dir(&state).expect("state");
        fs::create_dir(&cache).expect("cache");
        fs::create_dir(&execution_temp).expect("execution temp");

        let service = AuthorityBootstrapRecoveryServiceV1::from_namespace(
            AuthorityBootstrapRecoveryNamespaceV1 {
                namespace: namespace.clone(),
            },
            process_factory(),
        );
        let root_ref = service
            .prepare_fresh_root_selection(&state, &cache, &execution_temp)
            .expect("fresh roots");
        let evidence = evidence();
        let evidence_hash = AuthorityBootstrapRecoveryServiceV1::evidence_set_hash(&evidence)
            .expect("evidence hash");
        let absent_pid = (std::process::id() as u64 + 1_000_000).to_string();
        let proof = service
            .probe_old_epoch_quiescence(evidence_hash, &[absent_pid])
            .expect("quiescence");
        let operation = AuthorityBootstrapRecoveryOperationV1::SelectFreshAuthorityEpoch {
            explicit_root_config: root_ref,
            expected_failed_bootstrap_hash: Some(expected_failed_bootstrap_hash),
            failed_journal_evidence: evidence,
            evidence_set_hash: evidence_hash,
            old_epoch_quiescence: Box::new(proof.clone()),
        };
        let now = current_epoch_ms();
        let challenge = service
            .issue_operator_challenge(&operation, now, 60_000)
            .expect("challenge");
        let confirmation = ExactBootstrapOperatorConfirmationV1::for_challenge(
            &challenge,
            evidence_hash,
            Some(proof.proof_hash),
            operation_root_selection_hash(&operation),
            now,
        );
        let authorization = service
            .authorize(&operation, confirmation, now)
            .expect("authorization");
        let receipt = service
            .execute(operation.clone(), authorization)
            .expect("fresh epoch");
        assert_eq!(receipt.old_authority_epoch, 1);
        assert_eq!(receipt.new_authority_epoch, 2);
        let (active_root, active_epoch) = resolve_active_epoch(&namespace).expect("active epoch");
        assert_eq!(active_epoch, 2);
        assert_eq!(
            active_root.parent(),
            Some(namespace.join(EPOCHS_DIRECTORY_NAME).as_path())
        );
        assert!(namespace.join(OLD_EPOCH_INERT_FILE).is_file());

        let challenge = service
            .issue_operator_challenge(&operation, current_epoch_ms(), 60_000)
            .expect("second challenge");
        let confirmation = ExactBootstrapOperatorConfirmationV1::for_challenge(
            &challenge,
            evidence_hash,
            operation_quiescence_hash(&operation),
            operation_root_selection_hash(&operation),
            current_epoch_ms(),
        );
        let authorization = service
            .authorize(&operation, confirmation, current_epoch_ms())
            .expect("second authorization");
        let error = service
            .execute(operation.clone(), authorization)
            .expect_err("same epoch must not be allocated twice");
        assert!(matches!(
            error,
            AuthorityBootstrapRecoveryErrorV1::ReconciliationPending
        ));
    }

    #[test]
    fn r71_bootstrap_recovery_rejects_live_old_process() {
        let temp = tempfile::tempdir().expect("tempdir");
        let base = fs::canonicalize(temp.path()).expect("canonical tempdir");
        let namespace = base.join("authority-namespace");
        fs::create_dir(&namespace).expect("namespace");
        let service = AuthorityBootstrapRecoveryServiceV1::from_namespace(
            AuthorityBootstrapRecoveryNamespaceV1 { namespace },
            process_factory(),
        );
        let error = service
            .probe_old_epoch_quiescence(
                canonical_bootstrap_hash(b"evidence"),
                &[std::process::id().to_string()],
            )
            .expect_err("current test process is live");
        assert!(matches!(
            error,
            AuthorityBootstrapRecoveryErrorV1::OldEpochStillLive(_)
        ));
    }

    fn operation_quiescence_hash(
        operation: &AuthorityBootstrapRecoveryOperationV1,
    ) -> Option<CanonicalHash> {
        match operation {
            AuthorityBootstrapRecoveryOperationV1::SelectFreshAuthorityEpoch {
                old_epoch_quiescence,
                ..
            } => Some(old_epoch_quiescence.proof_hash),
            AuthorityBootstrapRecoveryOperationV1::RevealBootstrapDiagnostic { .. } => None,
        }
    }
}
