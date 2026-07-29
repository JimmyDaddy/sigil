use std::{
    collections::BTreeMap,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use super::*;
use crate::provider_connections::{PreparedCredential, ProviderFamily};

#[derive(Default)]
struct MemoryStore {
    records: Mutex<BTreeMap<CredentialId, ProviderCredentialRecord>>,
    unavailable: AtomicBool,
    rejected: bool,
    loads: AtomicUsize,
    stores: AtomicUsize,
    deletes: AtomicUsize,
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
    let legacy_native = Arc::new(MemoryStore::rejected());
    let file = Arc::new(MemoryStore::default());
    let store = ConfiguredProviderCredentialStore::injected(
        CredentialStorageMode::Auto,
        native.clone(),
        Some(legacy_native.clone()),
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
    assert_eq!(legacy_native.stores.load(Ordering::SeqCst), 0);
    assert_eq!(legacy_native.loads.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn auto_reads_a_silently_available_legacy_native_record_when_file_is_missing() {
    let legacy_native = Arc::new(MemoryStore::default());
    let file = Arc::new(MemoryStore::default());
    let record = record();
    legacy_native
        .store(&record)
        .await
        .expect("seed legacy native record");
    let store = ConfiguredProviderCredentialStore::injected(
        CredentialStorageMode::Auto,
        Arc::new(MemoryStore::rejected()),
        Some(legacy_native.clone()),
        Some(file.clone()),
    );

    let loaded = store
        .load(&record.credential_id)
        .await
        .expect("silent legacy load")
        .expect("legacy record");

    assert_eq!(loaded.secret().expose_secret(), "auto-secret");
    assert_eq!(file.loads.load(Ordering::SeqCst), 1);
    assert_eq!(legacy_native.loads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn auto_treats_unavailable_non_interactive_native_access_as_missing() {
    let store = ConfiguredProviderCredentialStore::injected(
        CredentialStorageMode::Auto,
        Arc::new(MemoryStore::rejected()),
        Some(Arc::new(MemoryStore::unavailable())),
        Some(Arc::new(MemoryStore::default())),
    );

    assert!(
        store
            .load(&CredentialId::random())
            .await
            .expect("authentication-required native record should be skipped")
            .is_none()
    );
}

#[tokio::test]
async fn auto_does_not_hide_a_rejected_silent_native_record() {
    let store = ConfiguredProviderCredentialStore::injected(
        CredentialStorageMode::Auto,
        Arc::new(MemoryStore::default()),
        Some(Arc::new(MemoryStore::rejected())),
        Some(Arc::new(MemoryStore::default())),
    );

    let error = store
        .load(&CredentialId::random())
        .await
        .expect_err("non-authentication native failures must remain visible");
    assert_eq!(
        error.code,
        ProviderCredentialErrorCode::CredentialStoreRejected.as_str()
    );
}

#[tokio::test]
async fn auto_delete_cleans_file_and_silently_accessible_legacy_native_records() {
    let legacy_native = Arc::new(MemoryStore::default());
    let file = Arc::new(MemoryStore::default());
    let record = record();
    legacy_native
        .store(&record)
        .await
        .expect("seed legacy native record");
    file.store(&record).await.expect("seed file record");
    let store = ConfiguredProviderCredentialStore::injected(
        CredentialStorageMode::Auto,
        Arc::new(MemoryStore::default()),
        Some(legacy_native.clone()),
        Some(file.clone()),
    );

    assert!(
        store
            .delete(&record.credential_id)
            .await
            .expect("clean both backends")
    );
    assert!(
        legacy_native
            .load(&record.credential_id)
            .await
            .expect("legacy native query")
            .is_none()
    );
    assert!(
        file.load(&record.credential_id)
            .await
            .expect("file query")
            .is_none()
    );
}

#[tokio::test]
async fn auto_delete_fails_closed_when_legacy_native_cleanup_cannot_be_verified() {
    let file = Arc::new(MemoryStore::default());
    let record = record();
    file.store(&record).await.expect("seed file record");
    let store = ConfiguredProviderCredentialStore::injected(
        CredentialStorageMode::Auto,
        Arc::new(MemoryStore::default()),
        Some(Arc::new(MemoryStore::unavailable())),
        Some(file.clone()),
    );

    let error = store
        .delete(&record.credential_id)
        .await
        .expect_err("unverified native cleanup must remain visible");
    assert_eq!(
        error.code,
        ProviderCredentialErrorCode::CredentialStoreUnavailable.as_str()
    );
    assert!(
        file.load(&record.credential_id)
            .await
            .expect("file query")
            .is_none()
    );
}

#[tokio::test]
async fn auto_without_a_platform_silent_store_stays_file_only() {
    let file = Arc::new(MemoryStore::default());
    let record = record();
    let store = ConfiguredProviderCredentialStore::injected(
        CredentialStorageMode::Auto,
        Arc::new(MemoryStore::rejected()),
        None,
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
    let legacy_native = Arc::new(MemoryStore::rejected());
    let file = Arc::new(MemoryStore::rejected());
    let record = record();
    let store = ConfiguredProviderCredentialStore::injected(
        CredentialStorageMode::Keyring,
        native.clone(),
        Some(legacy_native.clone()),
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
    assert_eq!(legacy_native.loads.load(Ordering::SeqCst), 0);
    assert_eq!(file.loads.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn explicit_file_mode_does_not_probe_native_storage() {
    let native = Arc::new(MemoryStore::rejected());
    let legacy_native = Arc::new(MemoryStore::rejected());
    let file = Arc::new(MemoryStore::default());
    let store = ConfiguredProviderCredentialStore::injected(
        CredentialStorageMode::File,
        native.clone(),
        Some(legacy_native.clone()),
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
    assert_eq!(legacy_native.loads.load(Ordering::SeqCst), 0);
}
