use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::Result;
use async_trait::async_trait;
use sigil_kernel::{
    ConfigPublishError, ConfigUpdateLockGuard, ConnectionId, CredentialStorageMode, ModelRef,
    RootConfig, atomic_publish_private_file,
};

use super::credential::keyed_secret_match;
use super::{
    ConfigMode, CredentialId, CredentialRefConfig, LoadedCredentialRef, PreparedCredential,
    ProviderConnectionConfig, ProviderCredentialError, ProviderCredentialRecord,
    ProviderCredentialStore, load_provider_connections, materialize_v2_root_config,
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
    pub confirmed_legacy_environment: BTreeSet<ConnectionId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyConnectionMigrationPreview {
    pub connection_count: usize,
    pub inline_credential_count: usize,
    pub environment_reference_count: usize,
    pub default_model: ModelRef,
}

#[derive(Debug, thiserror::Error)]
pub enum LegacyConnectionMigrationError {
    #[error("provider configuration is not legacy V1")]
    NotLegacy,
    #[error("legacy provider configuration is invalid")]
    InvalidConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyConnectionMigrationPublishStatus {
    Published,
    PublishedDurabilityUncertain,
    PublishedVisibilityReconciled,
}

#[derive(Debug, Clone)]
pub struct LegacyConnectionMigrationOutcome {
    pub root_config: RootConfig,
    pub status: LegacyConnectionMigrationPublishStatus,
    pub connection_count: usize,
    pub inline_credential_count: usize,
    pub environment_reference_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyMigrationRecoveryState {
    RollbackIncomplete,
    ReconcileRequired,
}

impl LegacyMigrationRecoveryState {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::RollbackIncomplete => "provider_migration_rollback_incomplete",
            Self::ReconcileRequired => "provider_migration_reconcile_required",
        }
    }

    const fn marker_label(self) -> &'static str {
        match self {
            Self::RollbackIncomplete => "rollback_incomplete",
            Self::ReconcileRequired => "reconcile_required",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct LegacyMigrationRecoveryRecord {
    state: LegacyMigrationRecoveryState,
    credential_store: Option<CredentialStorageMode>,
    orphaned_credential_ids: Vec<CredentialId>,
}

impl LegacyMigrationRecoveryRecord {
    fn encode(&self) -> Option<Vec<u8>> {
        let credential_store = self.credential_store?;
        let mut encoded = format!(
            "sigil-provider-migration-recovery-v3\n{}\ncredential_store={}\n",
            self.state.marker_label(),
            credential_store.as_str(),
        );
        for credential_id in &self.orphaned_credential_ids {
            encoded.push_str("orphan=");
            encoded.push_str(&credential_id.to_string());
            encoded.push('\n');
        }
        Some(encoded.into_bytes())
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        match bytes {
            b"sigil-provider-migration-recovery-v1\nrollback_incomplete\n" => {
                return Some(Self {
                    state: LegacyMigrationRecoveryState::RollbackIncomplete,
                    credential_store: None,
                    orphaned_credential_ids: Vec::new(),
                });
            }
            b"sigil-provider-migration-recovery-v1\nreconcile_required\n" => {
                return Some(Self {
                    state: LegacyMigrationRecoveryState::ReconcileRequired,
                    credential_store: None,
                    orphaned_credential_ids: Vec::new(),
                });
            }
            _ => {}
        }
        let raw = std::str::from_utf8(bytes).ok()?;
        let mut lines = raw.lines();
        let schema = lines.next()?;
        let state = match lines.next()? {
            "rollback_incomplete" => LegacyMigrationRecoveryState::RollbackIncomplete,
            "reconcile_required" => LegacyMigrationRecoveryState::ReconcileRequired,
            _ => return None,
        };
        let credential_store = match schema {
            "sigil-provider-migration-recovery-v2" => None,
            "sigil-provider-migration-recovery-v3" => {
                let value = lines.next()?.strip_prefix("credential_store=")?;
                Some(match value {
                    "auto" => CredentialStorageMode::Auto,
                    "keyring" => CredentialStorageMode::Keyring,
                    "file" => CredentialStorageMode::File,
                    _ => return None,
                })
            }
            _ => return None,
        };
        let mut orphaned_credential_ids = Vec::new();
        for line in lines {
            let value = line.strip_prefix("orphan=")?;
            let credential_id = CredentialId::parse(value).ok()?;
            if orphaned_credential_ids.contains(&credential_id) {
                return None;
            }
            orphaned_credential_ids.push(credential_id);
            if orphaned_credential_ids.len() > 128 {
                return None;
            }
        }
        Some(Self {
            state,
            credential_store,
            orphaned_credential_ids,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LegacyMigrationRecoveryError {
    #[error("provider migration recovery state is unavailable")]
    Unavailable,
    #[error("provider migration recovery state is invalid")]
    Invalid,
}

#[derive(Debug, thiserror::Error)]
pub enum LegacyConnectionMigrationTransactionError {
    #[error("legacy migration transaction lock failed")]
    TransactionLock,
    #[error("legacy migration configuration is temporarily unavailable")]
    ConfigUnavailable,
    #[error("legacy migration configuration could not be read")]
    ConfigRead,
    #[error("legacy migration preview is stale")]
    Stale,
    #[error("legacy migration is not required")]
    NotRequired,
    #[error("legacy migration is blocked by invalid configuration")]
    Blocked,
    #[error("legacy migration save failed")]
    Save {
        #[source]
        source: ConnectionSaveError,
    },
    #[error("legacy migration was not published")]
    NotPublished { rollback_incomplete: bool },
    #[error("legacy migration publication requires reconciliation")]
    ReconcileRequired,
    #[error("legacy migration recovery must be resolved before retrying")]
    RecoveryRequired { state: LegacyMigrationRecoveryState },
    #[error("legacy migration recovery state could not be persisted")]
    RecoveryStateUnavailable,
}

#[derive(Debug)]
struct LegacyConnectionMigrationPlan {
    draft: ConnectionSaveDraft,
    preview: LegacyConnectionMigrationPreview,
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

pub fn legacy_connection_migration_preview(
    current: &RootConfig,
) -> Result<LegacyConnectionMigrationPreview, LegacyConnectionMigrationError> {
    let loaded = load_provider_connections(current);
    if loaded.mode != ConfigMode::LegacyV1 {
        return Err(LegacyConnectionMigrationError::NotLegacy);
    }
    if !loaded.issues.is_empty() {
        return Err(LegacyConnectionMigrationError::InvalidConfig);
    }
    let default_model = loaded
        .default_model
        .ok_or(LegacyConnectionMigrationError::InvalidConfig)?;
    Ok(LegacyConnectionMigrationPreview {
        connection_count: loaded.connections.len(),
        inline_credential_count: loaded
            .connections
            .values()
            .filter(|connection| {
                matches!(connection.credential, LoadedCredentialRef::LegacyInline(_))
            })
            .count(),
        environment_reference_count: loaded
            .connections
            .values()
            .filter(|connection| {
                matches!(
                    connection.credential,
                    LoadedCredentialRef::Config(CredentialRefConfig::Environment { .. })
                )
            })
            .count(),
        default_model,
    })
}

/// Reads the durable, secret-free recovery block for one provider configuration.
///
/// # Errors
///
/// Returns an error when the marker is not a regular bounded file or has an unknown schema.
pub fn legacy_migration_recovery_state(
    config_path: &Path,
) -> Result<Option<LegacyMigrationRecoveryState>, LegacyMigrationRecoveryError> {
    Ok(read_legacy_migration_recovery(config_path)?.map(|record| record.state))
}

fn read_legacy_migration_recovery(
    config_path: &Path,
) -> Result<Option<LegacyMigrationRecoveryRecord>, LegacyMigrationRecoveryError> {
    let path = legacy_migration_recovery_path(config_path)?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(LegacyMigrationRecoveryError::Unavailable),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 8_192 {
        return Err(LegacyMigrationRecoveryError::Invalid);
    }
    let bytes = fs::read(&path).map_err(|_| LegacyMigrationRecoveryError::Unavailable)?;
    LegacyMigrationRecoveryRecord::decode(&bytes)
        .map(Some)
        .ok_or(LegacyMigrationRecoveryError::Invalid)
}

/// Clears a durable migration recovery block only after the caller has rebuilt an exact,
/// credential-aware inventory and reconciled every tracked credential ID.
///
/// Returns `true` when no recovery block remains.
///
/// # Errors
///
/// Returns an error when the marker is unsafe or cannot be removed.
pub(super) async fn clear_legacy_migration_recovery_if_healthy(
    config_path: &Path,
    expected_source: &[u8],
    root_config: &RootConfig,
    inventory: &super::ConnectionInventory,
    cleanup_store: RecoveryCredentialCleanupStore<'_>,
) -> Result<bool, LegacyMigrationRecoveryError> {
    let Some(recovery) = read_legacy_migration_recovery(config_path)? else {
        return Ok(true);
    };
    let healthy_v2 = inventory.mode == ConfigMode::V2
        && inventory.default_model.is_some()
        && inventory.issues.is_empty()
        && !inventory.entries.is_empty()
        && inventory.entries.iter().all(|entry| {
            entry.readiness == super::ConnectionReadiness::Ready && entry.issue.is_none()
        });
    let loaded = load_provider_connections(root_config);
    let valid_legacy_source = loaded.mode == ConfigMode::LegacyV1
        && loaded.default_model.is_some()
        && loaded.issues.is_empty();
    if (recovery.state == LegacyMigrationRecoveryState::ReconcileRequired && !healthy_v2)
        || (recovery.state == LegacyMigrationRecoveryState::RollbackIncomplete
            && !healthy_v2
            && !valid_legacy_source)
    {
        return Ok(false);
    }
    if recovery.state == LegacyMigrationRecoveryState::RollbackIncomplete
        && recovery.orphaned_credential_ids.is_empty()
    {
        return Ok(false);
    }
    let referenced_credential_ids = loaded
        .connections
        .values()
        .filter_map(|connection| match &connection.credential {
            LoadedCredentialRef::Config(
                CredentialRefConfig::SystemKeyring { id } | CredentialRefConfig::Stored { id },
            ) => Some(id.clone()),
            LoadedCredentialRef::Config(
                CredentialRefConfig::Environment { .. } | CredentialRefConfig::None,
            )
            | LoadedCredentialRef::LegacyInline(_) => None,
        })
        .collect::<BTreeSet<_>>();
    let _config_lock = ConfigUpdateLockGuard::acquire(config_path)
        .map_err(|_| LegacyMigrationRecoveryError::Unavailable)?;
    let live_source =
        fs::read(config_path).map_err(|_| LegacyMigrationRecoveryError::Unavailable)?;
    if live_source != expected_source || !legacy_migration_recovery_matches(config_path, &recovery)?
    {
        return Ok(false);
    }

    let unreferenced_credential_ids = recovery
        .orphaned_credential_ids
        .iter()
        .filter(|credential_id| !referenced_credential_ids.contains(*credential_id))
        .collect::<Vec<_>>();
    if !unreferenced_credential_ids.is_empty() {
        let native_store = match cleanup_store {
            RecoveryCredentialCleanupStore::Native => {
                let Some(mode) = recovery.credential_store else {
                    return Ok(false);
                };
                Some(super::ConfiguredProviderCredentialStore::from_storage_mode(
                    mode,
                ))
            }
            RecoveryCredentialCleanupStore::Injected(_) => None,
        };
        let credential_store: &dyn ProviderCredentialStore = match cleanup_store {
            RecoveryCredentialCleanupStore::Native => native_store
                .as_ref()
                .expect("native cleanup store was initialized"),
            RecoveryCredentialCleanupStore::Injected(store) => store,
        };
        for credential_id in unreferenced_credential_ids {
            if credential_store.delete(credential_id).await.is_err() {
                return Ok(false);
            }
        }
    }
    if !legacy_migration_recovery_matches(config_path, &recovery)? {
        return Ok(false);
    }
    remove_legacy_migration_recovery(config_path)?;
    Ok(true)
}

#[derive(Clone, Copy)]
pub(super) enum RecoveryCredentialCleanupStore<'a> {
    Native,
    #[allow(dead_code)]
    Injected(&'a dyn ProviderCredentialStore),
}

fn legacy_migration_recovery_matches(
    config_path: &Path,
    expected: &LegacyMigrationRecoveryRecord,
) -> Result<bool, LegacyMigrationRecoveryError> {
    Ok(read_legacy_migration_recovery(config_path)?.as_ref() == Some(expected))
}

fn remove_legacy_migration_recovery(
    config_path: &Path,
) -> Result<(), LegacyMigrationRecoveryError> {
    let path = legacy_migration_recovery_path(config_path)?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(LegacyMigrationRecoveryError::Unavailable),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(LegacyMigrationRecoveryError::Invalid);
    }
    fs::remove_file(path).map_err(|_| LegacyMigrationRecoveryError::Unavailable)?;
    Ok(())
}

/// Rebuilds one credential-aware inventory and resolves its durable migration recovery record.
///
/// Referenced credentials in a healthy V2 source are preserved; tracked unreferenced credentials
/// are deleted. An exact valid legacy source may clear only rollback recovery after cleanup, while
/// publication reconciliation continues to require healthy V2.
///
/// # Errors
///
/// Returns an error when the recovery record, credential store, or native verification worker is
/// unavailable.
pub fn recheck_legacy_migration_recovery_native(
    config_path: &Path,
    expected_source: &[u8],
    root_config: &RootConfig,
) -> Result<(bool, super::ConnectionInventory), LegacyMigrationRecoveryError> {
    let config_path = config_path.to_path_buf();
    let expected_source = expected_source.to_vec();
    let root_config = root_config.clone();
    std::thread::Builder::new()
        .name("sigil-provider-migration-recheck".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| LegacyMigrationRecoveryError::Unavailable)?;
            runtime.block_on(async move {
                let inventory_credential_store =
                    super::ConfiguredProviderCredentialStore::from_root_config(&root_config);
                let inventory = super::connection_inventory(
                    &root_config,
                    &inventory_credential_store,
                    &super::ProcessCredentialEnvironment,
                )
                .await;
                let cleared = clear_legacy_migration_recovery_if_healthy(
                    &config_path,
                    &expected_source,
                    &root_config,
                    &inventory,
                    RecoveryCredentialCleanupStore::Native,
                )
                .await?;
                Ok((cleared, inventory))
            })
        })
        .map_err(|_| LegacyMigrationRecoveryError::Unavailable)?
        .join()
        .map_err(|_| LegacyMigrationRecoveryError::Unavailable)?
}

fn legacy_migration_recovery_path(
    config_path: &Path,
) -> Result<PathBuf, LegacyMigrationRecoveryError> {
    let file_name = config_path
        .file_name()
        .ok_or(LegacyMigrationRecoveryError::Invalid)?;
    let mut marker_name = file_name.to_os_string();
    marker_name.push(".provider-migration-recovery-v1");
    Ok(config_path.with_file_name(marker_name))
}

fn persist_legacy_migration_recovery(
    config_path: &Path,
    state: LegacyMigrationRecoveryState,
    credential_store: CredentialStorageMode,
    orphaned_credential_ids: &[CredentialId],
) -> Result<(), LegacyMigrationRecoveryError> {
    if orphaned_credential_ids.len() > 128
        || orphaned_credential_ids
            .iter()
            .collect::<BTreeSet<_>>()
            .len()
            != orphaned_credential_ids.len()
    {
        return Err(LegacyMigrationRecoveryError::Invalid);
    }
    if state == LegacyMigrationRecoveryState::RollbackIncomplete
        && orphaned_credential_ids.is_empty()
    {
        return Err(LegacyMigrationRecoveryError::Invalid);
    }
    let path = legacy_migration_recovery_path(config_path)?;
    let record = LegacyMigrationRecoveryRecord {
        state,
        credential_store: Some(credential_store),
        orphaned_credential_ids: orphaned_credential_ids.to_vec(),
    };
    let encoded = record
        .encode()
        .ok_or(LegacyMigrationRecoveryError::Invalid)?;
    atomic_publish_private_file(&path, &encoded)
        .map_err(|_| LegacyMigrationRecoveryError::Unavailable)
}

fn prepare_legacy_connection_migration(
    current: &RootConfig,
) -> Result<LegacyConnectionMigrationPlan, LegacyConnectionMigrationError> {
    let preview = legacy_connection_migration_preview(current)?;
    let loaded = load_provider_connections(current);
    let mut connections = BTreeMap::new();
    let mut credential_updates = Vec::new();
    for (connection_id, loaded_connection) in loaded.connections {
        if let LoadedCredentialRef::LegacyInline(secret) = loaded_connection.credential {
            credential_updates.push(ConnectionCredentialUpdate {
                connection_id: connection_id.clone(),
                prepared: PreparedCredential::api_key_secret(
                    loaded_connection.config.provider,
                    secret,
                ),
            });
        }
        connections.insert(connection_id, loaded_connection.config);
    }
    Ok(LegacyConnectionMigrationPlan {
        draft: ConnectionSaveDraft {
            connections,
            default_model: preview.default_model.clone(),
            credential_updates,
            confirmed_legacy_environment: BTreeSet::new(),
        },
        preview,
    })
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
    created_credential_ids: Vec<CredentialId>,
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
    #[error("legacy_secret_migration_required for connection {connection_id}")]
    LegacySecretMigrationRequired { connection_id: ConnectionId },
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

enum ConnectionCompareAndSwap<'a> {
    Parsed,
    ExactSource(&'a [u8]),
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
    compare_and_swap: ConnectionCompareAndSwap<'_>,
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
            ConnectionCompareAndSwap::ExactSource(expected_source) => {
                let live = fs::read(path).map_err(|_| ConnectionSaveError::CurrentConfigInvalid)?;
                if live != expected_source {
                    return Err(ConnectionSaveError::ConcurrentModification);
                }
            }
        }
    }
    let current_loaded = load_provider_connections(current);
    if matches!(
        current_loaded.mode,
        ConfigMode::Mixed | ConfigMode::UnsupportedFuture
    ) || (current_loaded.mode == ConfigMode::LegacyV1 && !current_loaded.issues.is_empty())
    {
        return Err(ConnectionSaveError::CurrentConfigInvalid);
    }

    let legacy_inline_connections = current_loaded
        .connections
        .iter()
        .filter_map(|(id, connection)| {
            matches!(connection.credential, LoadedCredentialRef::LegacyInline(_))
                .then_some(id.clone())
        })
        .collect::<BTreeSet<_>>();
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
    for connection_id in &legacy_inline_connections {
        let confirmed_environment = draft.confirmed_legacy_environment.contains(connection_id)
            && draft
                .connections
                .get(connection_id)
                .is_some_and(|connection| {
                    matches!(
                        connection.credential,
                        CredentialRefConfig::Environment { .. }
                    )
                });
        if !update_ids.contains(connection_id) && !confirmed_environment {
            return Err(ConnectionSaveError::LegacySecretMigrationRequired {
                connection_id: connection_id.clone(),
            });
        }
    }

    let old_keyring_ids = current_loaded
        .connections
        .iter()
        .filter_map(|(id, connection)| match &connection.credential {
            LoadedCredentialRef::Config(
                CredentialRefConfig::SystemKeyring { id: credential_id }
                | CredentialRefConfig::Stored { id: credential_id },
            ) => Some((id.clone(), credential_id.clone())),
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

    let next = match materialize_v2_root_config(next_base, &draft.connections, &draft.default_model)
    {
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
            CredentialRefConfig::SystemKeyring { id } | CredentialRefConfig::Stored { id } => {
                Some(id.clone())
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let mut old_credential_cleanup_warning = false;
    if publish_outcome == ConfigPublishOutcome::Published {
        let retired_ids = old_keyring_ids
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
        created_credential_ids: created_ids,
    })
}

/// Migrates one exact persisted LegacyV1 source through the configured credential transaction.
///
/// `expected_source` never leaves the native/runtime owner. It is compared byte-for-byte while the
/// config update lock is held, before any credential record is written.
struct TrackingProviderCredentialStore<'a> {
    inner: &'a dyn ProviderCredentialStore,
    config_path: &'a Path,
    credential_store: CredentialStorageMode,
    attempted_store_ids: Mutex<Vec<CredentialId>>,
}

impl<'a> TrackingProviderCredentialStore<'a> {
    fn new(
        inner: &'a dyn ProviderCredentialStore,
        config_path: &'a Path,
        credential_store: CredentialStorageMode,
    ) -> Self {
        Self {
            inner,
            config_path,
            credential_store,
            attempted_store_ids: Mutex::new(Vec::new()),
        }
    }

    fn attempted_store_ids(&self) -> Vec<CredentialId> {
        self.attempted_store_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl ProviderCredentialStore for TrackingProviderCredentialStore<'_> {
    async fn load(
        &self,
        credential_id: &CredentialId,
    ) -> Result<Option<ProviderCredentialRecord>, ProviderCredentialError> {
        self.inner.load(credential_id).await
    }

    async fn store(
        &self,
        record: &ProviderCredentialRecord,
    ) -> Result<(), ProviderCredentialError> {
        let mut guarded_ids = self.attempted_store_ids();
        guarded_ids.push(record.credential_id.clone());
        persist_legacy_migration_recovery(
            self.config_path,
            LegacyMigrationRecoveryState::RollbackIncomplete,
            self.credential_store,
            &guarded_ids,
        )
        .map_err(|_| {
            ProviderCredentialError::new(
                super::ProviderCredentialErrorCode::CredentialStoreUnavailable,
                "provider migration recovery guard is unavailable",
            )
        })?;
        self.attempted_store_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(record.credential_id.clone());
        self.inner.store(record).await
    }

    async fn delete(&self, credential_id: &CredentialId) -> Result<bool, ProviderCredentialError> {
        self.inner.delete(credential_id).await
    }
}

pub async fn migrate_legacy_provider_config(
    path: &Path,
    expected_source: &[u8],
    credential_store: &dyn ProviderCredentialStore,
    publisher: &dyn ProviderConfigPublisher,
) -> Result<LegacyConnectionMigrationOutcome, LegacyConnectionMigrationTransactionError> {
    let transaction_lock = ConfigUpdateLockGuard::acquire(path)
        .map_err(|_| LegacyConnectionMigrationTransactionError::TransactionLock)?;
    if let Some(state) = legacy_migration_recovery_state(path)
        .map_err(|_| LegacyConnectionMigrationTransactionError::RecoveryStateUnavailable)?
    {
        return Err(LegacyConnectionMigrationTransactionError::RecoveryRequired { state });
    }
    let live_source = fs::read(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            LegacyConnectionMigrationTransactionError::Stale
        } else {
            LegacyConnectionMigrationTransactionError::ConfigUnavailable
        }
    })?;
    if live_source != expected_source {
        return Err(LegacyConnectionMigrationTransactionError::Stale);
    }
    let raw = std::str::from_utf8(&live_source)
        .map_err(|_| LegacyConnectionMigrationTransactionError::Blocked)?;
    let current = RootConfig::parse_persisted(raw)
        .map_err(|_| LegacyConnectionMigrationTransactionError::Blocked)?;
    let plan = match prepare_legacy_connection_migration(&current) {
        Ok(plan) => plan,
        Err(LegacyConnectionMigrationError::NotLegacy) => {
            return Err(LegacyConnectionMigrationTransactionError::NotRequired);
        }
        Err(LegacyConnectionMigrationError::InvalidConfig) => {
            return Err(LegacyConnectionMigrationTransactionError::Blocked);
        }
    };
    let credential_storage_mode = current.storage.credential_store;
    let tracking_store =
        TrackingProviderCredentialStore::new(credential_store, path, credential_storage_mode);
    let save = match save_connection_config_with_guard(
        &current,
        &current,
        path,
        plan.draft,
        &tracking_store,
        publisher,
        &transaction_lock,
        ConnectionCompareAndSwap::ExactSource(expected_source),
    )
    .await
    {
        Ok(save) => save,
        Err(ConnectionSaveError::ConcurrentModification) => {
            return Err(LegacyConnectionMigrationTransactionError::Stale);
        }
        Err(source) => {
            let guarded_ids = tracking_store.attempted_store_ids();
            if !guarded_ids.is_empty() && !connection_save_error_has_orphan(&source) {
                remove_legacy_migration_recovery(path).map_err(|_| {
                    LegacyConnectionMigrationTransactionError::RecoveryStateUnavailable
                })?;
            }
            return Err(LegacyConnectionMigrationTransactionError::Save { source });
        }
    };

    let status = match save.publish_outcome {
        ConfigPublishOutcome::Published => LegacyConnectionMigrationPublishStatus::Published,
        ConfigPublishOutcome::PublishedDurabilityUncertain => {
            LegacyConnectionMigrationPublishStatus::PublishedDurabilityUncertain
        }
        ConfigPublishOutcome::PublishedVisibilityUncertain { .. } => {
            match confirmed_migrated_root(path, &save.root_config) {
                Some(_) => LegacyConnectionMigrationPublishStatus::PublishedVisibilityReconciled,
                None if fs::read(path).ok().as_deref() == Some(expected_source) => {
                    let rollback_incomplete = rollback_created_credentials(
                        credential_store,
                        &save.created_credential_ids,
                    )
                    .await;
                    if !rollback_incomplete {
                        remove_legacy_migration_recovery(path).map_err(|_| {
                            LegacyConnectionMigrationTransactionError::RecoveryStateUnavailable
                        })?;
                    }
                    return Err(LegacyConnectionMigrationTransactionError::NotPublished {
                        rollback_incomplete,
                    });
                }
                None => {
                    persist_legacy_migration_recovery(
                        path,
                        LegacyMigrationRecoveryState::ReconcileRequired,
                        credential_storage_mode,
                        &save.created_credential_ids,
                    )
                    .map_err(|_| {
                        LegacyConnectionMigrationTransactionError::RecoveryStateUnavailable
                    })?;
                    return Err(LegacyConnectionMigrationTransactionError::ReconcileRequired);
                }
            }
        }
    };
    let Some(root_config) = confirmed_migrated_root(path, &save.root_config) else {
        persist_legacy_migration_recovery(
            path,
            LegacyMigrationRecoveryState::ReconcileRequired,
            credential_storage_mode,
            &save.created_credential_ids,
        )
        .map_err(|_| LegacyConnectionMigrationTransactionError::RecoveryStateUnavailable)?;
        return Err(LegacyConnectionMigrationTransactionError::ReconcileRequired);
    };
    remove_legacy_migration_recovery(path)
        .map_err(|_| LegacyConnectionMigrationTransactionError::RecoveryStateUnavailable)?;
    Ok(LegacyConnectionMigrationOutcome {
        root_config,
        status,
        connection_count: plan.preview.connection_count,
        inline_credential_count: plan.preview.inline_credential_count,
        environment_reference_count: plan.preview.environment_reference_count,
    })
}

fn connection_save_error_has_orphan(error: &ConnectionSaveError) -> bool {
    match error {
        ConnectionSaveError::CredentialStoreWrite {
            orphaned_credential,
            ..
        }
        | ConnectionSaveError::CredentialReadBackMismatch {
            orphaned_credential,
        }
        | ConnectionSaveError::Materialize {
            orphaned_credential,
            ..
        }
        | ConnectionSaveError::ConfigNotPublished {
            orphaned_credential,
            ..
        } => *orphaned_credential,
        ConnectionSaveError::TransactionLock { .. }
        | ConnectionSaveError::ConcurrentModification
        | ConnectionSaveError::LegacySecretMigrationRequired { .. }
        | ConnectionSaveError::CurrentConfigInvalid
        | ConnectionSaveError::DuplicateCredentialUpdate { .. }
        | ConnectionSaveError::ConnectionNotFound
        | ConnectionSaveError::CredentialProviderMismatch => false,
    }
}

fn confirmed_migrated_root(path: &Path, expected_root: &RootConfig) -> Option<RootConfig> {
    let root_config = RootConfig::load_persisted(path).ok()?;
    let actual = toml::to_string(&root_config).ok()?;
    let expected = toml::to_string(expected_root).ok()?;
    (actual == expected).then_some(root_config)
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

#[cfg(test)]
#[path = "../tests/provider_connection_persistence_tests.rs"]
mod tests;
