use std::sync::Mutex;

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
static CREDENTIAL_OPERATION_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemProviderCredentialStore;

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SilentSystemProviderCredentialStore;

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
        let bytes = run_keyring_task(move || {
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
        run_keyring_task(move || {
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
        run_keyring_task(move || {
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

#[cfg(target_os = "macos")]
#[async_trait]
impl ProviderCredentialStore for SilentSystemProviderCredentialStore {
    async fn load(
        &self,
        credential_id: &CredentialId,
    ) -> Result<Option<ProviderCredentialRecord>, ProviderCredentialError> {
        let expected = credential_id.clone();
        let account = keyring_account(credential_id);
        let bytes = run_keyring_task(move || silent_macos_load(&account)).await?;
        bytes
            .map(|bytes| decode_record(&expected, bytes.as_slice()))
            .transpose()
    }

    async fn store(
        &self,
        _record: &ProviderCredentialRecord,
    ) -> Result<(), ProviderCredentialError> {
        Err(ProviderCredentialError::new(
            ProviderCredentialErrorCode::CredentialStoreUnavailable,
            "non-interactive native credential access does not accept writes",
        ))
    }

    async fn delete(&self, credential_id: &CredentialId) -> Result<bool, ProviderCredentialError> {
        let account = keyring_account(credential_id);
        run_keyring_task(move || silent_macos_delete(&account)).await
    }
}

#[cfg(target_os = "macos")]
fn silent_macos_load(account: &str) -> Result<Option<Zeroizing<Vec<u8>>>, ProviderCredentialError> {
    use security_framework::item::SearchResult;

    let options = silent_macos_search(account, true)?;
    match options.search() {
        Ok(results) => match results.into_iter().next() {
            Some(SearchResult::Data(bytes)) => Ok(Some(Zeroizing::new(bytes))),
            None => Ok(None),
            Some(_) => Err(invalid_record()),
        },
        Err(error) if error.code() == MACOS_ERR_ITEM_NOT_FOUND => Ok(None),
        Err(error) => Err(map_silent_macos_error(error)),
    }
}

#[cfg(target_os = "macos")]
fn silent_macos_delete(account: &str) -> Result<bool, ProviderCredentialError> {
    let options = silent_macos_search(account, false)?;
    match options.delete() {
        Ok(()) => Ok(true),
        Err(error) if error.code() == MACOS_ERR_ITEM_NOT_FOUND => Ok(false),
        Err(error) => Err(map_silent_macos_error(error)),
    }
}

#[cfg(target_os = "macos")]
fn silent_macos_search(
    account: &str,
    load_data: bool,
) -> Result<security_framework::item::ItemSearchOptions, ProviderCredentialError> {
    use security_framework::{
        item::{ItemClass, ItemSearchOptions},
        os::macos::keychain::{SecKeychain, SecPreferencesDomain},
    };

    let keychain = SecKeychain::default_for_domain(SecPreferencesDomain::User)
        .map_err(map_silent_macos_error)?;
    let mut options = ItemSearchOptions::new();
    options
        .keychains(&[keychain])
        .class(ItemClass::generic_password())
        .service(CREDENTIAL_SERVICE)
        .account(account)
        // Keep the no-prompt guarantee local to this query. Process-global Keychain UI toggles
        // would also affect explicit `keyring`, MCP OAuth, and continuation credential owners.
        .skip_authenticated_items(true);
    if load_data {
        options.load_data(true).limit(1);
    }
    Ok(options)
}

#[cfg(target_os = "macos")]
const MACOS_ERR_NOT_AVAILABLE: i32 = -25291;
#[cfg(target_os = "macos")]
const MACOS_ERR_READ_ONLY: i32 = -25292;
#[cfg(target_os = "macos")]
const MACOS_ERR_AUTH_FAILED: i32 = -25293;
#[cfg(target_os = "macos")]
const MACOS_ERR_NO_SUCH_KEYCHAIN: i32 = -25294;
#[cfg(target_os = "macos")]
const MACOS_ERR_INVALID_KEYCHAIN: i32 = -25295;
#[cfg(target_os = "macos")]
const MACOS_ERR_ITEM_NOT_FOUND: i32 = -25300;
#[cfg(target_os = "macos")]
const MACOS_ERR_INTERACTION_NOT_ALLOWED: i32 = -25308;

#[cfg(target_os = "macos")]
fn map_silent_macos_error(error: security_framework::base::Error) -> ProviderCredentialError {
    let unavailable = matches!(
        error.code(),
        MACOS_ERR_NOT_AVAILABLE
            | MACOS_ERR_READ_ONLY
            | MACOS_ERR_AUTH_FAILED
            | MACOS_ERR_NO_SUCH_KEYCHAIN
            | MACOS_ERR_INVALID_KEYCHAIN
            | MACOS_ERR_INTERACTION_NOT_ALLOWED
    );
    if unavailable {
        ProviderCredentialError::new(
            ProviderCredentialErrorCode::CredentialStoreUnavailable,
            "native credential store requires authentication UI or is unavailable",
        )
    } else {
        ProviderCredentialError::new(
            ProviderCredentialErrorCode::CredentialStoreRejected,
            "native credential store rejected non-interactive access",
        )
    }
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_os = "linux"
))]
async fn run_keyring_task<T: Send + 'static>(
    task: impl FnOnce() -> Result<T, ProviderCredentialError> + Send + 'static,
) -> Result<T, ProviderCredentialError> {
    tokio::task::spawn_blocking(move || {
        let _operation = CREDENTIAL_OPERATION_LOCK.lock().map_err(|_| {
            ProviderCredentialError::new(
                ProviderCredentialErrorCode::CredentialStoreUnavailable,
                "native credential store operation lock is unavailable",
            )
        })?;
        task()
    })
    .await
    .map_err(|_| {
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
