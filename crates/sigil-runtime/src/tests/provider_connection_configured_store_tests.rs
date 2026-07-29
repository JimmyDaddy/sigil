use std::{
    collections::BTreeMap,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use super::*;
use crate::provider_connections::{PreparedCredential, ProviderFamily};

#[derive(Default)]
struct MemoryStore {
    records: Mutex<BTreeMap<CredentialId, ProviderCredentialRecord>>,
    rejected: bool,
    loads: AtomicUsize,
    stores: AtomicUsize,
    deletes: AtomicUsize,
}

impl MemoryStore {
    fn rejected() -> Self {
        Self {
            rejected: true,
            ..Self::default()
        }
    }

    fn preflight(&self) -> Result<(), ProviderCredentialError> {
        if self.rejected {
            return Err(ProviderCredentialError::new(
                ProviderCredentialErrorCode::CredentialStoreRejected,
                "test store rejected",
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl ProviderCredentialStore for MemoryStore {
    async fn load(
        &self,
        credential_id: &CredentialId,
    ) -> Result<Option<ProviderCredentialRecord>, ProviderCredentialError> {
        self.loads.fetch_add(1, Ordering::SeqCst);
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
        self.stores.fetch_add(1, Ordering::SeqCst);
        self.preflight()?;
        self.records
            .lock()
            .expect("memory store lock")
            .insert(record.credential_id.clone(), record.clone());
        Ok(())
    }

    async fn delete(&self, credential_id: &CredentialId) -> Result<bool, ProviderCredentialError> {
        self.deletes.fetch_add(1, Ordering::SeqCst);
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
async fn auto_writes_and_reads_file_without_probing_native_storage() {
    let native = Arc::new(MemoryStore::rejected());
    let file = Arc::new(MemoryStore::default());
    let store = ConfiguredProviderCredentialStore::injected(
        CredentialStorageMode::Auto,
        native.clone(),
        Some(file.clone()),
    );
    let record = record();

    store.store(&record).await.expect("auto file store");
    let loaded = store
        .load(&record.credential_id)
        .await
        .expect("auto file load")
        .expect("file record");

    assert_eq!(loaded.secret().expose_secret(), "auto-secret");
    assert_eq!(file.stores.load(Ordering::SeqCst), 1);
    assert_eq!(file.loads.load(Ordering::SeqCst), 1);
    assert_eq!(native.stores.load(Ordering::SeqCst), 0);
    assert_eq!(native.loads.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn auto_does_not_probe_native_storage_when_file_is_missing() {
    let native = Arc::new(MemoryStore::rejected());
    let file = Arc::new(MemoryStore::default());
    let store = ConfiguredProviderCredentialStore::injected(
        CredentialStorageMode::Auto,
        native.clone(),
        Some(file.clone()),
    );

    assert!(
        store
            .load(&CredentialId::random())
            .await
            .expect("missing file record")
            .is_none()
    );
    assert_eq!(file.loads.load(Ordering::SeqCst), 1);
    assert_eq!(native.loads.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn auto_delete_is_file_only_and_does_not_probe_native_storage() {
    let native = Arc::new(MemoryStore::rejected());
    let file = Arc::new(MemoryStore::default());
    let record = record();
    file.store(&record).await.expect("seed file record");
    let store = ConfiguredProviderCredentialStore::injected(
        CredentialStorageMode::Auto,
        native.clone(),
        Some(file.clone()),
    );

    assert!(
        store
            .delete(&record.credential_id)
            .await
            .expect("delete file record")
    );
    assert!(
        file.load(&record.credential_id)
            .await
            .expect("file query")
            .is_none()
    );
    assert_eq!(native.deletes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn auto_stays_file_only() {
    let file = Arc::new(MemoryStore::default());
    let record = record();
    let store = ConfiguredProviderCredentialStore::injected(
        CredentialStorageMode::Auto,
        Arc::new(MemoryStore::rejected()),
        Some(file),
    );

    store.store(&record).await.expect("auto file store");
    assert!(
        store
            .delete(&record.credential_id)
            .await
            .expect("file-only auto cleanup")
    );
}

#[tokio::test]
async fn explicit_keyring_mode_uses_only_the_interactive_native_store() {
    let native = Arc::new(MemoryStore::default());
    let file = Arc::new(MemoryStore::rejected());
    let record = record();
    let store = ConfiguredProviderCredentialStore::injected(
        CredentialStorageMode::Keyring,
        native.clone(),
        Some(file.clone()),
    );

    store.store(&record).await.expect("keyring store");
    assert!(
        store
            .load(&record.credential_id)
            .await
            .expect("keyring load")
            .is_some()
    );

    assert_eq!(native.stores.load(Ordering::SeqCst), 1);
    assert_eq!(native.loads.load(Ordering::SeqCst), 1);
    assert_eq!(file.loads.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn explicit_file_mode_does_not_probe_native_storage() {
    let native = Arc::new(MemoryStore::rejected());
    let file = Arc::new(MemoryStore::default());
    let store = ConfiguredProviderCredentialStore::injected(
        CredentialStorageMode::File,
        native.clone(),
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
    assert_eq!(native.loads.load(Ordering::SeqCst), 0);
}
