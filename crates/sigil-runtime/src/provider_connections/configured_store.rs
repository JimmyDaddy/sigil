use std::{fmt, sync::Arc};

use async_trait::async_trait;
use sigil_kernel::{CredentialStorageMode, RootConfig};

use super::{
    CredentialId, FileProviderCredentialStore, ProviderCredentialError,
    ProviderCredentialErrorCode, ProviderCredentialRecord, ProviderCredentialStore,
    SystemProviderCredentialStore,
};

#[cfg(target_os = "macos")]
use super::keyring_store::SilentSystemProviderCredentialStore;

/// Credential store selected by `[storage].credential_store`.
///
/// `auto` keeps the private credential file authoritative. On macOS it may read or clean up a
/// prior native record only through a query that forbids authentication UI.
#[derive(Clone)]
pub struct ConfiguredProviderCredentialStore {
    mode: CredentialStorageMode,
    native: Arc<dyn ProviderCredentialStore>,
    legacy_native: Option<Arc<dyn ProviderCredentialStore>>,
    file: Option<Arc<dyn ProviderCredentialStore>>,
}

impl ConfiguredProviderCredentialStore {
    #[must_use]
    pub fn from_root_config(root_config: &RootConfig) -> Self {
        Self::from_storage_mode(root_config.storage.credential_store)
    }

    #[must_use]
    pub(crate) fn from_storage_mode(mode: CredentialStorageMode) -> Self {
        let file = FileProviderCredentialStore::default_path()
            .ok()
            .map(|path| Arc::new(FileProviderCredentialStore::new(path)) as Arc<_>);
        let legacy_native = match mode {
            CredentialStorageMode::Auto => silent_legacy_native_store(),
            CredentialStorageMode::Keyring | CredentialStorageMode::File => None,
        };
        Self {
            mode,
            native: Arc::new(SystemProviderCredentialStore),
            legacy_native,
            file,
        }
    }

    #[cfg(test)]
    fn injected(
        mode: CredentialStorageMode,
        native: Arc<dyn ProviderCredentialStore>,
        legacy_native: Option<Arc<dyn ProviderCredentialStore>>,
        file: Option<Arc<dyn ProviderCredentialStore>>,
    ) -> Self {
        Self {
            mode,
            native,
            legacy_native,
            file,
        }
    }

    fn file(&self) -> Result<&dyn ProviderCredentialStore, ProviderCredentialError> {
        self.file.as_deref().ok_or_else(|| {
            ProviderCredentialError::new(
                ProviderCredentialErrorCode::CredentialStoreUnavailable,
                "Sigil credential file path is unavailable",
            )
        })
    }
}

impl fmt::Debug for ConfiguredProviderCredentialStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfiguredProviderCredentialStore")
            .field("mode", &self.mode)
            .field("native", &"[system credential store]")
            .field(
                "legacy_native",
                &self
                    .legacy_native
                    .as_ref()
                    .map(|_| "[non-interactive system credential store]"),
            )
            .field(
                "file",
                &self.file.as_ref().map(|_| "[private credential file]"),
            )
            .finish()
    }
}

#[async_trait]
impl ProviderCredentialStore for ConfiguredProviderCredentialStore {
    async fn load(
        &self,
        credential_id: &CredentialId,
    ) -> Result<Option<ProviderCredentialRecord>, ProviderCredentialError> {
        match self.mode {
            CredentialStorageMode::Keyring => self.native.load(credential_id).await,
            CredentialStorageMode::File => self.file()?.load(credential_id).await,
            CredentialStorageMode::Auto => {
                if let Some(record) = self.file()?.load(credential_id).await? {
                    return Ok(Some(record));
                }
                let Some(legacy_native) = self.legacy_native.as_deref() else {
                    return Ok(None);
                };
                match legacy_native.load(credential_id).await {
                    Ok(record) => Ok(record),
                    Err(error) if error_is_unavailable(&error) => Ok(None),
                    Err(error) => Err(error),
                }
            }
        }
    }

    async fn store(
        &self,
        record: &ProviderCredentialRecord,
    ) -> Result<(), ProviderCredentialError> {
        match self.mode {
            CredentialStorageMode::Keyring => self.native.store(record).await,
            CredentialStorageMode::File | CredentialStorageMode::Auto => {
                self.file()?.store(record).await
            }
        }
    }

    async fn delete(&self, credential_id: &CredentialId) -> Result<bool, ProviderCredentialError> {
        match self.mode {
            CredentialStorageMode::Keyring => self.native.delete(credential_id).await,
            CredentialStorageMode::File => self.file()?.delete(credential_id).await,
            CredentialStorageMode::Auto => {
                let file_deleted = self.file()?.delete(credential_id).await?;
                let Some(legacy_native) = self.legacy_native.as_deref() else {
                    return Ok(file_deleted);
                };
                legacy_native
                    .delete(credential_id)
                    .await
                    .map(|native_deleted| file_deleted || native_deleted)
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn silent_legacy_native_store() -> Option<Arc<dyn ProviderCredentialStore>> {
    Some(Arc::new(SilentSystemProviderCredentialStore))
}

#[cfg(not(target_os = "macos"))]
fn silent_legacy_native_store() -> Option<Arc<dyn ProviderCredentialStore>> {
    None
}

fn error_is_unavailable(error: &ProviderCredentialError) -> bool {
    error.code == ProviderCredentialErrorCode::CredentialStoreUnavailable.as_str()
}

#[cfg(test)]
#[path = "../tests/provider_connection_configured_store_tests.rs"]
mod tests;
