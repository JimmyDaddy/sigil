use std::fmt;

use async_trait::async_trait;
use ring::{hmac, rand::SecureRandom};
use serde::{Deserialize, Serialize};
use sigil_kernel::SecretString;
use uuid::Uuid;

use super::{CredentialId, CredentialRefConfig, ProviderConnectionConfig, ProviderFamily};

const PROVIDER_CREDENTIAL_RECORD_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialAuthKind {
    ApiKey,
}

impl CredentialAuthKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApiKey => "api_key",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CredentialGenerationId(Uuid);

impl CredentialGenerationId {
    #[must_use]
    pub fn random() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for CredentialGenerationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone)]
pub struct PreparedCredential {
    pub provider_family: ProviderFamily,
    pub auth_kind: CredentialAuthKind,
    secret: SecretString,
}

impl PreparedCredential {
    #[must_use]
    pub fn api_key(provider_family: ProviderFamily, value: impl Into<String>) -> Self {
        Self {
            provider_family,
            auth_kind: CredentialAuthKind::ApiKey,
            secret: SecretString::new(value),
        }
    }

    #[must_use]
    pub fn secret(&self) -> &SecretString {
        &self.secret
    }
}

impl fmt::Debug for PreparedCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedCredential")
            .field("provider_family", &self.provider_family)
            .field("auth_kind", &self.auth_kind)
            .field("secret", &"[redacted]")
            .finish()
    }
}

#[derive(Clone)]
pub struct ProviderCredentialRecord {
    pub version: u32,
    pub credential_id: CredentialId,
    pub provider_family: ProviderFamily,
    pub auth_kind: CredentialAuthKind,
    pub generation_id: CredentialGenerationId,
    secret: SecretString,
}

impl ProviderCredentialRecord {
    #[must_use]
    pub fn new(credential_id: CredentialId, prepared: &PreparedCredential) -> Self {
        Self {
            version: PROVIDER_CREDENTIAL_RECORD_VERSION,
            credential_id,
            provider_family: prepared.provider_family,
            auth_kind: prepared.auth_kind,
            generation_id: CredentialGenerationId::random(),
            secret: prepared.secret.clone(),
        }
    }

    #[must_use]
    pub fn secret(&self) -> &SecretString {
        &self.secret
    }

    pub(crate) fn from_decoded(
        credential_id: CredentialId,
        provider_family: ProviderFamily,
        auth_kind: CredentialAuthKind,
        generation_id: CredentialGenerationId,
        secret: SecretString,
    ) -> Self {
        Self {
            version: PROVIDER_CREDENTIAL_RECORD_VERSION,
            credential_id,
            provider_family,
            auth_kind,
            generation_id,
            secret,
        }
    }
}

impl fmt::Debug for ProviderCredentialRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderCredentialRecord")
            .field("version", &self.version)
            .field("credential_id", &"[redacted]")
            .field("provider_family", &self.provider_family)
            .field("auth_kind", &self.auth_kind)
            .field("generation_id", &"[redacted]")
            .field("secret", &"[redacted]")
            .finish()
    }
}

#[async_trait]
pub trait ProviderCredentialStore: Send + Sync {
    async fn load(
        &self,
        credential_id: &CredentialId,
    ) -> Result<Option<ProviderCredentialRecord>, ProviderCredentialError>;

    async fn store(&self, record: &ProviderCredentialRecord)
    -> Result<(), ProviderCredentialError>;

    async fn delete(&self, credential_id: &CredentialId) -> Result<bool, ProviderCredentialError>;
}

pub trait CredentialEnvironment: Send + Sync {
    fn read(&self, name: &str) -> Option<SecretString>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessCredentialEnvironment;

impl CredentialEnvironment for ProcessCredentialEnvironment {
    fn read(&self, name: &str) -> Option<SecretString> {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .map(SecretString::new)
    }
}

#[derive(Clone)]
pub enum LoadedCredentialRef {
    Config(CredentialRefConfig),
    LegacyInline(SecretString),
}

impl fmt::Debug for LoadedCredentialRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(CredentialRefConfig::Environment { name }) => formatter
                .debug_struct("Environment")
                .field("name", name)
                .finish(),
            Self::Config(CredentialRefConfig::SystemKeyring { .. }) => {
                formatter.write_str("SystemKeyring([redacted])")
            }
            Self::Config(CredentialRefConfig::Stored { .. }) => {
                formatter.write_str("Stored([redacted])")
            }
            Self::Config(CredentialRefConfig::None) => formatter.write_str("None"),
            Self::LegacyInline(_) => formatter.write_str("LegacyInline([redacted])"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedCredentialSource {
    Environment,
    SystemKeyring,
    Stored,
    None,
    LegacyInline,
    ProcessStaged,
}

#[derive(Clone)]
pub struct ResolvedCredential {
    pub secret: Option<SecretString>,
    pub source: ResolvedCredentialSource,
    pub generation_id: Option<CredentialGenerationId>,
}

impl fmt::Debug for ResolvedCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedCredential")
            .field("secret", &self.secret.as_ref().map(|_| "[redacted]"))
            .field("source", &self.source)
            .field(
                "generation_id",
                &self.generation_id.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderCredentialErrorCode {
    CredentialMissing,
    CredentialStoreUnavailable,
    CredentialStoreRejected,
    CredentialRecordInvalid,
    CredentialRecordMismatch,
}

impl ProviderCredentialErrorCode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CredentialMissing => "credential_missing",
            Self::CredentialStoreUnavailable => "credential_store_unavailable",
            Self::CredentialStoreRejected => "credential_store_rejected",
            Self::CredentialRecordInvalid => "credential_record_invalid",
            Self::CredentialRecordMismatch => "credential_record_mismatch",
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{code}: {message}")]
pub struct ProviderCredentialError {
    pub code: &'static str,
    message: &'static str,
}

impl ProviderCredentialError {
    #[must_use]
    pub fn new(code: ProviderCredentialErrorCode, message: &'static str) -> Self {
        Self {
            code: code.as_str(),
            message,
        }
    }
}

pub async fn resolve_connection_credential(
    connection: &ProviderConnectionConfig,
    loaded_ref: &LoadedCredentialRef,
    store: &dyn ProviderCredentialStore,
    environment: &dyn CredentialEnvironment,
) -> Result<ResolvedCredential, ProviderCredentialError> {
    match loaded_ref {
        LoadedCredentialRef::LegacyInline(secret) => Ok(ResolvedCredential {
            secret: Some(secret.clone()),
            source: ResolvedCredentialSource::LegacyInline,
            generation_id: None,
        }),
        LoadedCredentialRef::Config(CredentialRefConfig::Environment { name }) => {
            let secret = environment.read(name).ok_or_else(|| {
                ProviderCredentialError::new(
                    ProviderCredentialErrorCode::CredentialMissing,
                    "credential environment value is missing",
                )
            })?;
            Ok(ResolvedCredential {
                secret: Some(secret),
                source: ResolvedCredentialSource::Environment,
                generation_id: None,
            })
        }
        LoadedCredentialRef::Config(
            credential @ (CredentialRefConfig::SystemKeyring { .. }
            | CredentialRefConfig::Stored { .. }),
        ) => {
            let (id, source) = match credential {
                CredentialRefConfig::SystemKeyring { id } => {
                    (id, ResolvedCredentialSource::SystemKeyring)
                }
                CredentialRefConfig::Stored { id } => (id, ResolvedCredentialSource::Stored),
                CredentialRefConfig::Environment { .. } | CredentialRefConfig::None => {
                    unreachable!("stored credential pattern was checked")
                }
            };
            let record = store.load(id).await?.ok_or_else(|| {
                ProviderCredentialError::new(
                    ProviderCredentialErrorCode::CredentialMissing,
                    "stored credential record is missing",
                )
            })?;
            if record.credential_id != *id
                || record.provider_family != connection.provider
                || record.auth_kind != CredentialAuthKind::ApiKey
            {
                return Err(ProviderCredentialError::new(
                    ProviderCredentialErrorCode::CredentialRecordMismatch,
                    "stored credential record does not match the connection",
                ));
            }
            Ok(ResolvedCredential {
                secret: Some(record.secret.clone()),
                source,
                generation_id: Some(record.generation_id),
            })
        }
        LoadedCredentialRef::Config(CredentialRefConfig::None) => Ok(ResolvedCredential {
            secret: None,
            source: ResolvedCredentialSource::None,
            generation_id: None,
        }),
    }
}

pub(crate) fn keyed_secret_match(left: &SecretString, right: &SecretString) -> bool {
    let mut key_bytes = [0_u8; 32];
    if ring::rand::SystemRandom::new()
        .fill(&mut key_bytes)
        .is_err()
    {
        return false;
    }
    let key = hmac::Key::new(hmac::HMAC_SHA256, &key_bytes);
    let left_tag = hmac::sign(&key, left.expose_secret().as_bytes());
    hmac::verify(&key, right.expose_secret().as_bytes(), left_tag.as_ref()).is_ok()
}
