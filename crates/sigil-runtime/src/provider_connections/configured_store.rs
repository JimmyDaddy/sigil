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
/// `auto` is intentionally identical to `file`. Native credential access is reserved for explicit
/// `keyring` mode because even a non-interactive platform query can block on the host credential
/// service.
#[derive(Clone)]
pub struct ConfiguredProviderCredentialStore {
    mode: CredentialStorageMode,
    native: Arc<dyn ProviderCredentialStore>,
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
        Self {
            mode,
            native: Arc::new(SystemProviderCredentialStore),
            file,
        }
    }

    #[cfg(test)]
    fn injected(
        mode: CredentialStorageMode,
        native: Arc<dyn ProviderCredentialStore>,
        file: Option<Arc<dyn ProviderCredentialStore>>,
    ) -> Self {
        Self { mode, native, file }
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
            CredentialStorageMode::File | CredentialStorageMode::Auto => {
                self.file()?.load(credential_id).await
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
            CredentialStorageMode::File | CredentialStorageMode::Auto => {
                self.file()?.delete(credential_id).await
            }
        }
    }
}

#[cfg(test)]
#[path = "../tests/provider_connection_configured_store_tests.rs"]
mod tests;
