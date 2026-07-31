use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::Result;
use sigil_kernel::{ConfigPublishError, ConfigUpdateLockGuard, ConnectionId, ModelRef, RootConfig};

use super::credential::keyed_secret_match;
use super::{
    ConfigMode, CredentialId, CredentialRefConfig, LoadedCredentialRef, PreparedCredential,
    ProviderConnectionConfig, ProviderCredentialError, ProviderCredentialRecord,
    ProviderCredentialStore, load_provider_connections, materialize_root_config,
};

#[derive(Debug)]
pub struct ConnectionCredentialUpdate {
    pub connection_id: ConnectionId,
    pub prepared: PreparedCredential,
}

#[derive(Debug)]
pub struct ConnectionSaveDraft {
    pub connections: BTreeMap<ConnectionId, ProviderConnectionConfig>,
    pub default_model: ModelRef,
    pub credential_updates: Vec<ConnectionCredentialUpdate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigPublishOutcome {
    Published,
    PublishedDurabilityUncertain,
    PublishedVisibilityUncertain { recovery_path: Option<PathBuf> },
}

pub trait ProviderConfigPublisher: Send + Sync {
    fn publish(
        &self,
        path: &Path,
        config: &RootConfig,
        lock: &ConfigUpdateLockGuard,
    ) -> Result<ConfigPublishOutcome, anyhow::Error>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RootConfigPublisher;

impl ProviderConfigPublisher for RootConfigPublisher {
    fn publish(
        &self,
        path: &Path,
        config: &RootConfig,
        lock: &ConfigUpdateLockGuard,
    ) -> Result<ConfigPublishOutcome, anyhow::Error> {
        match config.save_with_update_lock(path, lock) {
            Ok(()) => Ok(ConfigPublishOutcome::Published),
            Err(error)
                if matches!(
                    error.downcast_ref::<ConfigPublishError>(),
                    Some(ConfigPublishError::PublishedButDurabilityUncertain { .. })
                ) =>
            {
                Ok(ConfigPublishOutcome::PublishedDurabilityUncertain)
            }
            Err(error) => match error.downcast_ref::<ConfigPublishError>() {
                Some(ConfigPublishError::ReplacementPartiallyApplied {
                    recovery_path,
                    previous_path,
                    ..
                }) => Ok(ConfigPublishOutcome::PublishedVisibilityUncertain {
                    recovery_path: previous_path
                        .clone()
                        .or_else(|| recovery_path.exists().then(|| recovery_path.clone())),
                }),
                Some(ConfigPublishError::PublishedButVisibilityUncertain { .. }) => {
                    Ok(ConfigPublishOutcome::PublishedVisibilityUncertain {
                        recovery_path: None,
                    })
                }
                _ => Err(error),
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConnectionSaveOutcome {
    pub root_config: RootConfig,
    pub publish_outcome: ConfigPublishOutcome,
    pub old_credential_cleanup_warning: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectionSaveError {
    #[error("config update transaction lock failed")]
    TransactionLock {
        #[source]
        source: anyhow::Error,
    },
    #[error("config changed since Provider settings were loaded; reload and retry")]
    ConcurrentModification,
    #[error("current provider connection config is invalid")]
    CurrentConfigInvalid,
    #[error("duplicate credential update for connection {connection_id}")]
    DuplicateCredentialUpdate { connection_id: ConnectionId },
    #[error("connection_not_found")]
    ConnectionNotFound,
    #[error("credential provider family does not match the connection")]
    CredentialProviderMismatch,
    #[error("credential store write failed")]
    CredentialStoreWrite {
        #[source]
        source: ProviderCredentialError,
        orphaned_credential: bool,
    },
    #[error("credential read-back verification failed")]
    CredentialReadBackMismatch { orphaned_credential: bool },
    #[error("failed to materialize V2 config")]
    Materialize {
        #[source]
        source: anyhow::Error,
        orphaned_credential: bool,
    },
    #[error("config was not published")]
    ConfigNotPublished {
        #[source]
        source: anyhow::Error,
        orphaned_credential: bool,
    },
}

pub async fn save_connection_config(
    current: &RootConfig,
    path: &Path,
    draft: ConnectionSaveDraft,
    credential_store: &dyn ProviderCredentialStore,
    publisher: &dyn ProviderConfigPublisher,
) -> Result<ConnectionSaveOutcome, ConnectionSaveError> {
    save_connection_config_with_base(current, current, path, draft, credential_store, publisher)
        .await
}

/// Saves a connection draft while preserving non-provider edits from `next_base`.
///
/// `current` is the exact config snapshot used for the transaction compare-and-swap. Keeping it
/// separate from `next_base` prevents an ordinary settings edit from being mistaken for an
/// external concurrent modification while still refusing a genuinely stale editor snapshot.
pub async fn save_connection_config_with_base(
    current: &RootConfig,
    next_base: &RootConfig,
    path: &Path,
    draft: ConnectionSaveDraft,
    credential_store: &dyn ProviderCredentialStore,
    publisher: &dyn ProviderConfigPublisher,
) -> Result<ConnectionSaveOutcome, ConnectionSaveError> {
    let transaction_lock = ConfigUpdateLockGuard::acquire(path)
        .map_err(|source| ConnectionSaveError::TransactionLock { source })?;
    save_connection_config_with_guard(
        current,
        next_base,
        path,
        draft,
        credential_store,
        publisher,
        &transaction_lock,
        ConnectionCompareAndSwap::Parsed,
    )
    .await
}

/// Replaces an existing unreadable or invalid configuration with one explicit current-schema
/// provider setup.
///
/// The live file is checked again while holding the cross-process update lock. A concurrently
/// repaired valid configuration is never overwritten.
pub async fn save_connection_config_replacing_invalid(
    replacement_base: &RootConfig,
    path: &Path,
    draft: ConnectionSaveDraft,
    credential_store: &dyn ProviderCredentialStore,
    publisher: &dyn ProviderConfigPublisher,
) -> Result<ConnectionSaveOutcome, ConnectionSaveError> {
    let transaction_lock = ConfigUpdateLockGuard::acquire(path)
        .map_err(|source| ConnectionSaveError::TransactionLock { source })?;
    save_connection_config_with_guard(
        replacement_base,
        replacement_base,
        path,
        draft,
        credential_store,
        publisher,
        &transaction_lock,
        ConnectionCompareAndSwap::Invalid,
    )
    .await
}

#[derive(Clone, Copy)]
enum ConnectionCompareAndSwap {
    Parsed,
    Invalid,
}

#[allow(clippy::too_many_arguments)]
async fn save_connection_config_with_guard(
    current: &RootConfig,
    next_base: &RootConfig,
    path: &Path,
    mut draft: ConnectionSaveDraft,
    credential_store: &dyn ProviderCredentialStore,
    publisher: &dyn ProviderConfigPublisher,
    transaction_lock: &ConfigUpdateLockGuard,
    compare_and_swap: ConnectionCompareAndSwap,
) -> Result<ConnectionSaveOutcome, ConnectionSaveError> {
    if path.exists() {
        match compare_and_swap {
            ConnectionCompareAndSwap::Parsed => {
                let live = RootConfig::load_persisted(path)
                    .map_err(|_| ConnectionSaveError::CurrentConfigInvalid)?;
                let expected = toml::to_string(current)
                    .map_err(|_| ConnectionSaveError::CurrentConfigInvalid)?;
                let actual = toml::to_string(&live)
                    .map_err(|_| ConnectionSaveError::CurrentConfigInvalid)?;
                if expected != actual {
                    return Err(ConnectionSaveError::ConcurrentModification);
                }
            }
            ConnectionCompareAndSwap::Invalid => {
                let live = fs::read_to_string(path)
                    .map_err(|_| ConnectionSaveError::CurrentConfigInvalid)?;
                if RootConfig::parse_persisted(&live).is_ok() {
                    return Err(ConnectionSaveError::ConcurrentModification);
                }
            }
        }
    } else if matches!(compare_and_swap, ConnectionCompareAndSwap::Invalid) {
        return Err(ConnectionSaveError::ConcurrentModification);
    }
    let current_loaded = load_provider_connections(current);
    if current_loaded.mode != ConfigMode::V2 || !current_loaded.issues.is_empty() {
        return Err(ConnectionSaveError::CurrentConfigInvalid);
    }

    let mut update_ids = BTreeSet::new();
    for update in &draft.credential_updates {
        if !update_ids.insert(update.connection_id.clone()) {
            return Err(ConnectionSaveError::DuplicateCredentialUpdate {
                connection_id: update.connection_id.clone(),
            });
        }
        let connection = draft
            .connections
            .get(&update.connection_id)
            .ok_or(ConnectionSaveError::ConnectionNotFound)?;
        if connection.provider != update.prepared.provider_family {
            return Err(ConnectionSaveError::CredentialProviderMismatch);
        }
    }
    let old_stored_ids = current_loaded
        .connections
        .iter()
        .filter_map(|(id, connection)| match &connection.credential {
            LoadedCredentialRef::Config(CredentialRefConfig::Stored { id: credential_id }) => {
                Some((id.clone(), credential_id.clone()))
            }
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    let mut created_ids = Vec::new();
    for update in draft.credential_updates {
        let connection = draft
            .connections
            .get_mut(&update.connection_id)
            .expect("credential updates were preflighted");
        let credential_id = CredentialId::random();
        let record = ProviderCredentialRecord::new(credential_id.clone(), &update.prepared);
        if let Err(source) = credential_store.store(&record).await {
            let mut cleanup_ids = created_ids.clone();
            cleanup_ids.push(credential_id);
            let orphaned_credential =
                rollback_created_credentials(credential_store, &cleanup_ids).await;
            return Err(ConnectionSaveError::CredentialStoreWrite {
                source,
                orphaned_credential,
            });
        }
        created_ids.push(credential_id.clone());
        let read_back = match credential_store.load(&credential_id).await {
            Ok(Some(record)) => record,
            Ok(None) | Err(_) => {
                let orphaned_credential =
                    rollback_created_credentials(credential_store, &created_ids).await;
                return Err(ConnectionSaveError::CredentialReadBackMismatch {
                    orphaned_credential,
                });
            }
        };
        if read_back.credential_id != credential_id
            || read_back.provider_family != record.provider_family
            || read_back.auth_kind != record.auth_kind
            || read_back.generation_id != record.generation_id
            || !keyed_secret_match(record.secret(), read_back.secret())
        {
            let orphaned_credential =
                rollback_created_credentials(credential_store, &created_ids).await;
            return Err(ConnectionSaveError::CredentialReadBackMismatch {
                orphaned_credential,
            });
        }

        connection.credential = CredentialRefConfig::Stored { id: credential_id };
    }

    let next = match materialize_root_config(next_base, &draft.connections, &draft.default_model) {
        Ok(config) => config,
        Err(source) => {
            let orphaned_credential =
                rollback_created_credentials(credential_store, &created_ids).await;
            return Err(ConnectionSaveError::Materialize {
                source,
                orphaned_credential,
            });
        }
    };

    let publish_outcome = match publisher.publish(path, &next, transaction_lock) {
        Ok(outcome) => outcome,
        Err(source) => {
            let orphaned_credential =
                rollback_created_credentials(credential_store, &created_ids).await;
            return Err(ConnectionSaveError::ConfigNotPublished {
                source,
                orphaned_credential,
            });
        }
    };

    let next_references = draft
        .connections
        .values()
        .filter_map(|connection| match &connection.credential {
            CredentialRefConfig::Stored { id } => Some(id.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let mut old_credential_cleanup_warning = false;
    if publish_outcome == ConfigPublishOutcome::Published {
        let retired_ids = old_stored_ids
            .values()
            .filter(|old_id| !next_references.contains(*old_id))
            .cloned()
            .collect::<BTreeSet<_>>();
        for old_id in &retired_ids {
            if credential_store.delete(old_id).await.is_err() {
                old_credential_cleanup_warning = true;
            }
        }
    }

    Ok(ConnectionSaveOutcome {
        root_config: next,
        publish_outcome,
        old_credential_cleanup_warning,
    })
}

async fn rollback_created_credentials(
    credential_store: &dyn ProviderCredentialStore,
    credential_ids: &[CredentialId],
) -> bool {
    let mut orphaned = false;
    for credential_id in credential_ids.iter().rev() {
        match credential_store.delete(credential_id).await {
            Ok(true | false) => {}
            Err(_) => orphaned = true,
        }
    }
    orphaned
}
