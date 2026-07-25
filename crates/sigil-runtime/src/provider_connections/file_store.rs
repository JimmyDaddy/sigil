use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use sigil_kernel::private_path_permissions_are_restricted;
use sigil_kernel::{
    atomic_publish_private_file, default_user_config_dir, secure_private_path_permissions,
};
use zeroize::{Zeroize, Zeroizing};

use super::{
    CredentialId, ProviderCredentialError, ProviderCredentialErrorCode, ProviderCredentialRecord,
    ProviderCredentialStore,
    keyring_store::{decode_record, encode_record},
};

const CREDENTIAL_FILE_VERSION: u32 = 1;
const CREDENTIAL_FILE_NAME: &str = "credentials.json";
const CREDENTIAL_LOCK_NAME: &str = "credentials.lock";
const CREDENTIAL_FILE_MAX_BYTES: u64 = 1024 * 1024;
const CREDENTIAL_FILE_MAX_RECORDS: usize = 512;
const ENCODED_RECORD_MAX_BYTES: usize = 4_096;

/// Owner-only plaintext credential file used when explicitly selected or as `auto` fallback.
///
/// The file is intentionally separate from `sigil.toml`, sessions, caches, and support data. Its
/// parent and files are tightened to owner-only permissions before use.
#[derive(Debug, Clone)]
pub struct FileProviderCredentialStore {
    path: PathBuf,
}

impl FileProviderCredentialStore {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn default_path() -> Result<PathBuf, ProviderCredentialError> {
        default_user_config_dir()
            .map(|directory| directory.join(CREDENTIAL_FILE_NAME))
            .map_err(|_| store_unavailable("Sigil credential file path is unavailable"))
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[async_trait]
impl ProviderCredentialStore for FileProviderCredentialStore {
    async fn load(
        &self,
        credential_id: &CredentialId,
    ) -> Result<Option<ProviderCredentialRecord>, ProviderCredentialError> {
        let path = self.path.clone();
        let expected = credential_id.clone();
        spawn_file_task(move || {
            with_credential_file_lock(&path, false, || {
                let wire = read_credential_file(&path)?;
                let Some(encoded) = wire.records.get(&expected.to_string()) else {
                    return Ok(None);
                };
                let bytes = STANDARD_NO_PAD
                    .decode(encoded.expose())
                    .map(Zeroizing::new)
                    .map_err(|_| invalid_record())?;
                decode_record(&expected, bytes.as_slice()).map(Some)
            })
        })
        .await
    }

    async fn store(
        &self,
        record: &ProviderCredentialRecord,
    ) -> Result<(), ProviderCredentialError> {
        let path = self.path.clone();
        let record = record.clone();
        spawn_file_task(move || {
            with_credential_file_lock(&path, true, || {
                let mut wire = read_credential_file(&path)?;
                let encoded = encode_record(&record)?;
                wire.records.insert(
                    record.credential_id.to_string(),
                    EncodedCredential::new(STANDARD_NO_PAD.encode(encoded.as_slice())),
                );
                write_credential_file(&path, &wire)
            })
        })
        .await
    }

    async fn delete(&self, credential_id: &CredentialId) -> Result<bool, ProviderCredentialError> {
        let path = self.path.clone();
        let credential_id = credential_id.to_string();
        spawn_file_task(move || {
            with_credential_file_lock(&path, true, || {
                let mut wire = read_credential_file(&path)?;
                let removed = wire.records.remove(&credential_id).is_some();
                if removed {
                    write_credential_file(&path, &wire)?;
                }
                Ok(removed)
            })
        })
        .await
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialFileWire {
    version: u32,
    records: BTreeMap<String, EncodedCredential>,
}

impl Default for CredentialFileWire {
    fn default() -> Self {
        Self {
            version: CREDENTIAL_FILE_VERSION,
            records: BTreeMap::new(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(transparent)]
struct EncodedCredential(String);

impl EncodedCredential {
    fn new(value: String) -> Self {
        Self(value)
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl Drop for EncodedCredential {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

async fn spawn_file_task<T: Send + 'static>(
    task: impl FnOnce() -> Result<T, ProviderCredentialError> + Send + 'static,
) -> Result<T, ProviderCredentialError> {
    tokio::task::spawn_blocking(task)
        .await
        .map_err(|_| store_unavailable("Sigil credential file operation did not complete"))?
}

fn with_credential_file_lock<T>(
    path: &Path,
    exclusive: bool,
    operation: impl FnOnce() -> Result<T, ProviderCredentialError>,
) -> Result<T, ProviderCredentialError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| store_unavailable("Sigil credential file has no parent directory"))?;
    fs::create_dir_all(parent)
        .map_err(|_| store_unavailable("Sigil credential directory could not be created"))?;
    secure_private_path_permissions(parent)
        .map_err(|_| store_unavailable("Sigil credential directory could not be secured"))?;
    let lock_path = parent.join(CREDENTIAL_LOCK_NAME);
    let lock = open_private_lock_file(&lock_path)?;
    if exclusive {
        FileExt::lock_exclusive(&lock)
    } else {
        FileExt::lock_shared(&lock)
    }
    .map_err(|_| store_unavailable("Sigil credential file lock is unavailable"))?;
    let result = operation();
    let unlock = FileExt::unlock(&lock)
        .map_err(|_| store_unavailable("Sigil credential file lock could not be released"));
    match (result, unlock) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

fn open_private_lock_file(path: &Path) -> Result<File, ProviderCredentialError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|_| store_unavailable("Sigil credential file lock could not be opened"))?;
    secure_private_path_permissions(path)
        .map_err(|_| store_unavailable("Sigil credential file lock could not be secured"))?;
    Ok(file)
}

fn read_credential_file(path: &Path) -> Result<CredentialFileWire, ProviderCredentialError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CredentialFileWire::default());
        }
        Err(_) => {
            return Err(store_unavailable(
                "Sigil credential file could not be inspected",
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(store_rejected(
            "Sigil credential path is not a regular file",
        ));
    }
    if metadata.len() > CREDENTIAL_FILE_MAX_BYTES {
        return Err(store_rejected(
            "Sigil credential file exceeds its size limit",
        ));
    }
    secure_private_path_permissions(path)
        .map_err(|_| store_unavailable("Sigil credential file could not be secured"))?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(path)
        .map_err(|_| store_unavailable("Sigil credential file could not be opened"))?;
    let capacity = usize::try_from(metadata.len())
        .map_err(|_| store_rejected("Sigil credential file exceeds its size limit"))?;
    let mut bytes = Zeroizing::new(Vec::with_capacity(capacity));
    file.by_ref()
        .take(CREDENTIAL_FILE_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| store_unavailable("Sigil credential file could not be read"))?;
    if bytes.len() as u64 > CREDENTIAL_FILE_MAX_BYTES {
        return Err(store_rejected(
            "Sigil credential file exceeds its size limit",
        ));
    }
    let wire: CredentialFileWire = serde_json::from_slice(&bytes).map_err(|_| invalid_record())?;
    validate_credential_file(&wire)?;
    Ok(wire)
}

fn validate_credential_file(wire: &CredentialFileWire) -> Result<(), ProviderCredentialError> {
    if wire.version != CREDENTIAL_FILE_VERSION || wire.records.len() > CREDENTIAL_FILE_MAX_RECORDS {
        return Err(invalid_record());
    }
    for (credential_id, encoded) in &wire.records {
        CredentialId::parse(credential_id).map_err(|_| invalid_record())?;
        if encoded.expose().is_empty() || encoded.expose().len() > ENCODED_RECORD_MAX_BYTES {
            return Err(invalid_record());
        }
    }
    Ok(())
}

fn write_credential_file(
    path: &Path,
    wire: &CredentialFileWire,
) -> Result<(), ProviderCredentialError> {
    validate_credential_file(wire)?;
    let bytes = serde_json::to_vec_pretty(wire)
        .map(Zeroizing::new)
        .map_err(|_| invalid_record())?;
    if bytes.len() as u64 > CREDENTIAL_FILE_MAX_BYTES {
        return Err(store_rejected(
            "Sigil credential file exceeds its size limit",
        ));
    }
    atomic_publish_private_file(path, bytes.as_slice())
        .map_err(|_| store_unavailable("Sigil credential file could not be published"))
}

fn invalid_record() -> ProviderCredentialError {
    ProviderCredentialError::new(
        ProviderCredentialErrorCode::CredentialRecordInvalid,
        "Sigil credential file is malformed or unsupported",
    )
}

fn store_unavailable(message: &'static str) -> ProviderCredentialError {
    ProviderCredentialError::new(
        ProviderCredentialErrorCode::CredentialStoreUnavailable,
        message,
    )
}

fn store_rejected(message: &'static str) -> ProviderCredentialError {
    ProviderCredentialError::new(
        ProviderCredentialErrorCode::CredentialStoreRejected,
        message,
    )
}

#[cfg(test)]
#[path = "../tests/provider_connection_file_store_tests.rs"]
mod tests;
