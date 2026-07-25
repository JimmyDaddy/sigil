use std::{fmt, sync::Arc};

use async_trait::async_trait;
use sigil_kernel::{CredentialStorageMode, RootConfig};

use super::{
    CredentialId, FileProviderCredentialStore, ProviderCredentialError,
    ProviderCredentialErrorCode, ProviderCredentialRecord, ProviderCredentialStore,
    SystemProviderCredentialStore,
};

/// Credential store selected by `[storage].credential_store`.
///
/// `auto` falls back only when the native store is unavailable. Rejected or malformed native
/// records remain hard failures rather than being hidden by a second backend.
#[derive(Clone)]
pub struct ConfiguredProviderCredentialStore {
    mode: CredentialStorageMode,
    keyring: Arc<dyn ProviderCredentialStore>,
    file: Option<Arc<dyn ProviderCredentialStore>>,
}

impl ConfiguredProviderCredentialStore {
    #[must_use]
    pub fn from_root_config(root_config: &RootConfig) -> Self {
        let file = FileProviderCredentialStore::default_path()
            .ok()
            .map(|path| Arc::new(FileProviderCredentialStore::new(path)) as Arc<_>);
        Self {
            mode: root_config.storage.credential_store,
            keyring: Arc::new(SystemProviderCredentialStore),
            file,
        }
    }

    #[cfg(test)]
    fn injected(
        mode: CredentialStorageMode,
        keyring: Arc<dyn ProviderCredentialStore>,
        file: Option<Arc<dyn ProviderCredentialStore>>,
    ) -> Self {
        Self {
            mode,
            keyring,
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
            .field("keyring", &"[system credential store]")
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
            CredentialStorageMode::Keyring => self.keyring.load(credential_id).await,
            CredentialStorageMode::File => self.file()?.load(credential_id).await,
            CredentialStorageMode::Auto => match self.keyring.load(credential_id).await {
                Ok(Some(record)) => Ok(Some(record)),
                Ok(None) => match self.file.as_deref() {
                    Some(file) => file.load(credential_id).await,
                    None => Ok(None),
                },
                Err(error) if error_is_unavailable(&error) => {
                    self.file()?.load(credential_id).await
                }
                Err(error) => Err(error),
            },
        }
    }

    async fn store(
        &self,
        record: &ProviderCredentialRecord,
    ) -> Result<(), ProviderCredentialError> {
        match self.mode {
            CredentialStorageMode::Keyring => self.keyring.store(record).await,
            CredentialStorageMode::File => self.file()?.store(record).await,
            CredentialStorageMode::Auto => match self.keyring.store(record).await {
                Ok(()) => Ok(()),
                Err(error) if error_is_unavailable(&error) => self.file()?.store(record).await,
                Err(error) => Err(error),
            },
        }
    }

    async fn delete(&self, credential_id: &CredentialId) -> Result<bool, ProviderCredentialError> {
        match self.mode {
            CredentialStorageMode::Keyring => self.keyring.delete(credential_id).await,
            CredentialStorageMode::File => self.file()?.delete(credential_id).await,
            CredentialStorageMode::Auto => match self.keyring.delete(credential_id).await {
                Ok(keyring_deleted) => match self.file.as_deref() {
                    Some(file) => file
                        .delete(credential_id)
                        .await
                        .map(|file_deleted| keyring_deleted || file_deleted),
                    None => Ok(keyring_deleted),
                },
                Err(error) if error_is_unavailable(&error) => {
                    self.file()?.delete(credential_id).await
                }
                Err(error) => Err(error),
            },
        }
    }
}

fn error_is_unavailable(error: &ProviderCredentialError) -> bool {
    error.code == ProviderCredentialErrorCode::CredentialStoreUnavailable.as_str()
}

#[cfg(test)]
#[path = "../tests/provider_connection_configured_store_tests.rs"]
mod tests;
