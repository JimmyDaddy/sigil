use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sigil_kernel::SecretString;
use uuid::Uuid;
use zeroize::Zeroize;
use zeroize::Zeroizing;

use super::{
    CredentialAuthKind, CredentialId, ProviderCredentialError, ProviderCredentialErrorCode,
    ProviderCredentialRecord, ProviderCredentialStore, ProviderFamily,
};

const CREDENTIAL_SERVICE: &str = "dev.sigil.provider-credential.v1";
const CREDENTIAL_RECORD_VERSION: u32 = 1;
const CREDENTIAL_RECORD_MAX_BYTES: usize = 2_560;
const CREDENTIAL_OPERATION_TIMEOUT: Duration = Duration::from_secs(5);
static CREDENTIAL_OPERATION_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemProviderCredentialStore;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialRecordWire {
    version: u32,
    credential_id: String,
    provider_family: ProviderFamily,
    auth_kind: CredentialAuthKind,
    generation_id: String,
    secret: String,
}

impl Drop for CredentialRecordWire {
    fn drop(&mut self) {
        self.secret.zeroize();
    }
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_os = "linux"
))]
#[async_trait]
impl ProviderCredentialStore for SystemProviderCredentialStore {
    async fn load(
        &self,
        credential_id: &CredentialId,
    ) -> Result<Option<ProviderCredentialRecord>, ProviderCredentialError> {
        let expected = credential_id.clone();
        let account = keyring_account(credential_id);
        let bytes = run_keyring_task(Some(CREDENTIAL_OPERATION_TIMEOUT), move || {
            let entry = keyring::Entry::new(CREDENTIAL_SERVICE, &account).map_err(|_| {
                ProviderCredentialError::new(
                    ProviderCredentialErrorCode::CredentialStoreUnavailable,
                    "native credential store is unavailable",
                )
            })?;
            match entry.get_secret() {
                Ok(bytes) => Ok(Some(Zeroizing::new(bytes))),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(error) => Err(map_keyring_read_error(&error)),
            }
        })
        .await?;
        bytes
            .map(|bytes| decode_record(&expected, bytes.as_slice()))
            .transpose()
    }

    async fn store(
        &self,
        record: &ProviderCredentialRecord,
    ) -> Result<(), ProviderCredentialError> {
        let account = keyring_account(&record.credential_id);
        let bytes = encode_record(record)?;
        run_keyring_task(None, move || {
            let entry = keyring::Entry::new(CREDENTIAL_SERVICE, &account).map_err(|_| {
                ProviderCredentialError::new(
                    ProviderCredentialErrorCode::CredentialStoreUnavailable,
                    "native credential store is unavailable",
                )
            })?;
            entry
                .set_secret(bytes.as_slice())
                .map_err(|error| map_keyring_write_error(&error))
        })
        .await
    }

    async fn delete(&self, credential_id: &CredentialId) -> Result<bool, ProviderCredentialError> {
        let account = keyring_account(credential_id);
        run_keyring_task(None, move || {
            let entry = keyring::Entry::new(CREDENTIAL_SERVICE, &account).map_err(|_| {
                ProviderCredentialError::new(
                    ProviderCredentialErrorCode::CredentialStoreUnavailable,
                    "native credential store is unavailable",
                )
            })?;
            match entry.delete_credential() {
                Ok(()) => Ok(true),
                Err(keyring::Error::NoEntry) => Ok(false),
                Err(error) => Err(map_keyring_delete_error(&error)),
            }
        })
        .await
    }
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_os = "linux"
))]
struct CredentialOperationGuard;

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_os = "linux"
))]
impl Drop for CredentialOperationGuard {
    fn drop(&mut self) {
        CREDENTIAL_OPERATION_IN_FLIGHT.store(false, Ordering::Release);
    }
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_os = "linux"
))]
async fn run_keyring_task<T: Send + 'static>(
    timeout: Option<Duration>,
    task: impl FnOnce() -> Result<T, ProviderCredentialError> + Send + 'static,
) -> Result<T, ProviderCredentialError> {
    CREDENTIAL_OPERATION_IN_FLIGHT
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| {
            ProviderCredentialError::new(
                ProviderCredentialErrorCode::CredentialStoreRejected,
                "another native credential store operation is still in progress",
            )
        })?;
    let guard = CredentialOperationGuard;
    let worker = tokio::task::spawn_blocking(move || {
        let _guard = guard;
        task()
    });
    let completed = if let Some(timeout) = timeout {
        tokio::time::timeout(timeout, worker).await.map_err(|_| {
            ProviderCredentialError::new(
                ProviderCredentialErrorCode::CredentialStoreUnavailable,
                "native credential store read timed out",
            )
        })?
    } else {
        worker.await
    };
    completed.map_err(|_| {
        ProviderCredentialError::new(
            ProviderCredentialErrorCode::CredentialStoreUnavailable,
            "credential store task did not complete",
        )
    })?
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_os = "linux"
)))]
#[async_trait]
impl ProviderCredentialStore for SystemProviderCredentialStore {
    async fn load(
        &self,
        _credential_id: &CredentialId,
    ) -> Result<Option<ProviderCredentialRecord>, ProviderCredentialError> {
        Err(store_unavailable())
    }

    async fn store(
        &self,
        _record: &ProviderCredentialRecord,
    ) -> Result<(), ProviderCredentialError> {
        Err(store_unavailable())
    }

    async fn delete(&self, _credential_id: &CredentialId) -> Result<bool, ProviderCredentialError> {
        Err(store_unavailable())
    }
}

pub(super) fn encode_record(
    record: &ProviderCredentialRecord,
) -> Result<Zeroizing<Vec<u8>>, ProviderCredentialError> {
    let wire = CredentialRecordWire {
        version: record.version,
        credential_id: record.credential_id.to_string(),
        provider_family: record.provider_family,
        auth_kind: record.auth_kind,
        generation_id: record.generation_id.to_string(),
        secret: record.secret().expose_secret().to_owned(),
    };
    let bytes = serde_json::to_vec(&wire).map_err(|_| invalid_record())?;
    if bytes.len() > CREDENTIAL_RECORD_MAX_BYTES {
        return Err(ProviderCredentialError::new(
            ProviderCredentialErrorCode::CredentialStoreRejected,
            "credential record exceeds the native cross-platform size limit",
        ));
    }
    Ok(Zeroizing::new(bytes))
}

pub(super) fn decode_record(
    expected_id: &CredentialId,
    bytes: &[u8],
) -> Result<ProviderCredentialRecord, ProviderCredentialError> {
    if bytes.len() > CREDENTIAL_RECORD_MAX_BYTES {
        return Err(invalid_record());
    }
    let mut wire: CredentialRecordWire =
        serde_json::from_slice(bytes).map_err(|_| invalid_record())?;
    if wire.version != CREDENTIAL_RECORD_VERSION {
        return Err(invalid_record());
    }
    let credential_id = CredentialId::parse(&wire.credential_id).map_err(|_| invalid_record())?;
    if &credential_id != expected_id {
        return Err(ProviderCredentialError::new(
            ProviderCredentialErrorCode::CredentialRecordMismatch,
            "credential record identity does not match the requested record",
        ));
    }
    let generation_uuid = Uuid::parse_str(&wire.generation_id).map_err(|_| invalid_record())?;
    if generation_uuid.get_version_num() != 4 {
        return Err(invalid_record());
    }
    let generation_id =
        serde_json::from_value(serde_json::Value::String(generation_uuid.to_string()))
            .map_err(|_| invalid_record())?;
    if wire.secret.is_empty() {
        return Err(invalid_record());
    }
    let secret = SecretString::new(std::mem::take(&mut wire.secret));
    Ok(ProviderCredentialRecord::from_decoded(
        credential_id,
        wire.provider_family,
        wire.auth_kind,
        generation_id,
        secret,
    ))
}

fn keyring_account(credential_id: &CredentialId) -> String {
    format!("provider:{}", credential_id)
}

fn invalid_record() -> ProviderCredentialError {
    ProviderCredentialError::new(
        ProviderCredentialErrorCode::CredentialRecordInvalid,
        "credential record is malformed or unsupported",
    )
}

fn keyring_error_is_unavailable(error: &keyring::Error) -> bool {
    matches!(error, keyring::Error::NoStorageAccess(_))
}

fn map_keyring_read_error(error: &keyring::Error) -> ProviderCredentialError {
    if keyring_error_is_unavailable(error) {
        ProviderCredentialError::new(
            ProviderCredentialErrorCode::CredentialStoreUnavailable,
            "native credential store could not read the record",
        )
    } else {
        ProviderCredentialError::new(
            ProviderCredentialErrorCode::CredentialStoreRejected,
            "native credential store rejected the read",
        )
    }
}

fn map_keyring_write_error(error: &keyring::Error) -> ProviderCredentialError {
    if keyring_error_is_unavailable(error) {
        ProviderCredentialError::new(
            ProviderCredentialErrorCode::CredentialStoreUnavailable,
            "native credential store is unavailable",
        )
    } else {
        ProviderCredentialError::new(
            ProviderCredentialErrorCode::CredentialStoreRejected,
            "native credential store rejected the record",
        )
    }
}

fn map_keyring_delete_error(error: &keyring::Error) -> ProviderCredentialError {
    if keyring_error_is_unavailable(error) {
        ProviderCredentialError::new(
            ProviderCredentialErrorCode::CredentialStoreUnavailable,
            "native credential store is unavailable",
        )
    } else {
        ProviderCredentialError::new(
            ProviderCredentialErrorCode::CredentialStoreRejected,
            "native credential store rejected credential cleanup",
        )
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_os = "linux"
)))]
fn store_unavailable() -> ProviderCredentialError {
    ProviderCredentialError::new(
        ProviderCredentialErrorCode::CredentialStoreUnavailable,
        "native credential store is unavailable on this platform",
    )
}

#[cfg(test)]
#[path = "../tests/provider_connection_keyring_store_tests.rs"]
mod tests;
