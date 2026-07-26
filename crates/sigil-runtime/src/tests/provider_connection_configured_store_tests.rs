use std::{
    collections::BTreeMap,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use super::*;
use crate::provider_connections::{PreparedCredential, ProviderFamily};

#[derive(Default)]
struct MemoryStore {
    records: Mutex<BTreeMap<CredentialId, ProviderCredentialRecord>>,
    unavailable: AtomicBool,
    rejected: bool,
}

impl MemoryStore {
    fn unavailable() -> Self {
        Self {
            unavailable: AtomicBool::new(true),
            ..Self::default()
        }
    }

    fn rejected() -> Self {
        Self {
            rejected: true,
            ..Self::default()
        }
    }

    fn preflight(&self) -> Result<(), ProviderCredentialError> {
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(ProviderCredentialError::new(
                ProviderCredentialErrorCode::CredentialStoreUnavailable,
                "test store unavailable",
            ));
        }
        if self.rejected {
            return Err(ProviderCredentialError::new(
                ProviderCredentialErrorCode::CredentialStoreRejected,
                "test store rejected",
            ));
        }
        Ok(())
    }

    fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }
}

#[async_trait]
impl ProviderCredentialStore for MemoryStore {
    async fn load(
        &self,
        credential_id: &CredentialId,
    ) -> Result<Option<ProviderCredentialRecord>, ProviderCredentialError> {
        self.preflight()?;
        Ok(self
            .records
            .lock()
            .expect("memory store lock")
            .get(credential_id)
            .cloned())
    }

    async fn store(
        &self,
        record: &ProviderCredentialRecord,
    ) -> Result<(), ProviderCredentialError> {
        self.preflight()?;
        self.records
            .lock()
            .expect("memory store lock")
            .insert(record.credential_id.clone(), record.clone());
        Ok(())
    }

    async fn delete(&self, credential_id: &CredentialId) -> Result<bool, ProviderCredentialError> {
        self.preflight()?;
        Ok(self
            .records
            .lock()
            .expect("memory store lock")
            .remove(credential_id)
            .is_some())
    }
}

fn record() -> ProviderCredentialRecord {
    ProviderCredentialRecord::new(
        CredentialId::random(),
        &PreparedCredential::api_key(ProviderFamily::OpenAi, "auto-secret"),
    )
}

#[tokio::test]
async fn auto_falls_back_only_when_native_store_is_unavailable() {
    let file = Arc::new(MemoryStore::default());
    let store = ConfiguredProviderCredentialStore::injected(
        CredentialStorageMode::Auto,
        Arc::new(MemoryStore::unavailable()),
        Some(file.clone()),
    );
    let record = record();

    store.store(&record).await.expect("file fallback store");
    let loaded = store
        .load(&record.credential_id)
        .await
        .expect("file fallback load")
        .expect("fallback record");
    assert_eq!(loaded.secret().expose_secret(), "auto-secret");
    let delete_error = store
        .delete(&record.credential_id)
        .await
        .expect_err("auto delete must not claim full cleanup while keyring is unavailable");
    assert_eq!(
        delete_error.code,
        ProviderCredentialErrorCode::CredentialStoreUnavailable.as_str()
    );
    assert!(
        file.load(&record.credential_id)
            .await
            .expect("file fallback query")
            .is_none()
    );

    let rejected = ConfiguredProviderCredentialStore::injected(
        CredentialStorageMode::Auto,
        Arc::new(MemoryStore::rejected()),
        Some(file),
    );
    assert!(rejected.store(&record).await.is_err());
}

#[tokio::test]
async fn auto_delete_waits_for_native_store_recovery_before_claiming_cleanup() {
    let keyring = Arc::new(MemoryStore::default());
    let file = Arc::new(MemoryStore::default());
    let store = ConfiguredProviderCredentialStore::injected(
        CredentialStorageMode::Auto,
        keyring.clone(),
        Some(file),
    );
    let record = record();

    store
        .store(&record)
        .await
        .expect("native store should accept");
    keyring.set_unavailable(true);
    let error = store
        .delete(&record.credential_id)
        .await
        .expect_err("temporary native outage must keep cleanup fail-closed");
    assert_eq!(
        error.code,
        ProviderCredentialErrorCode::CredentialStoreUnavailable.as_str()
    );

    keyring.set_unavailable(false);
    assert!(
        store
            .delete(&record.credential_id)
            .await
            .expect("recovered native store should complete cleanup")
    );
}

#[tokio::test]
async fn explicit_file_mode_does_not_probe_the_native_store() {
    let file = Arc::new(MemoryStore::default());
    let store = ConfiguredProviderCredentialStore::injected(
        CredentialStorageMode::File,
        Arc::new(MemoryStore::rejected()),
        Some(file),
    );
    let record = record();

    store.store(&record).await.expect("file-only store");
    assert!(
        store
            .load(&record.credential_id)
            .await
            .expect("file-only load")
            .is_some()
    );
}

#[tokio::test]
async fn auto_reads_and_cleans_a_prior_file_fallback_when_native_store_recovers() {
    let keyring = Arc::new(MemoryStore::default());
    let file = Arc::new(MemoryStore::default());
    let record = record();
    file.store(&record).await.expect("seed file fallback");
    let store = ConfiguredProviderCredentialStore::injected(
        CredentialStorageMode::Auto,
        keyring.clone(),
        Some(file.clone()),
    );

    assert!(
        store
            .load(&record.credential_id)
            .await
            .expect("auto load")
            .is_some()
    );
    keyring
        .store(&record)
        .await
        .expect("seed recovered keyring");
    assert!(
        store
            .delete(&record.credential_id)
            .await
            .expect("clean both backends")
    );
    assert!(
        keyring
            .load(&record.credential_id)
            .await
            .expect("keyring query")
            .is_none()
    );
    assert!(
        file.load(&record.credential_id)
            .await
            .expect("file query")
            .is_none()
    );
}
