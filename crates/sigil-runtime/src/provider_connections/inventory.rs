use std::sync::atomic::{AtomicBool, Ordering};

use sigil_kernel::RootConfig;

use super::{
    ConnectionInventory, ConnectionInventoryEntry, ConnectionIssueView, ConnectionReadiness,
    CredentialEnvironment, CredentialRefConfig, CredentialSourceView, LoadedCredentialRef,
    ProviderCredentialErrorCode, ProviderCredentialStore, load_provider_connections,
};

/// Builds an offline, secret-free inventory. Stored credential entries remain unverified.
#[must_use]
pub fn connection_inventory_offline(
    root_config: &RootConfig,
    environment: &dyn CredentialEnvironment,
) -> ConnectionInventory {
    let loaded = load_provider_connections(root_config);
    let default_model = loaded.default_model.clone();
    let entries = loaded
        .connections
        .values()
        .map(|connection| {
            let (credential_source, readiness, issue) =
                offline_credential_state(&connection.credential, environment);
            ConnectionInventoryEntry {
                id: connection.config.id.clone(),
                label: connection.config.label.clone(),
                provider_label: connection.config.provider.label().to_owned(),
                protocol_label: connection.config.protocol.label().to_owned(),
                endpoint_display: endpoint_display(&connection.config.base_url),
                credential_source,
                readiness: if default_model
                    .as_ref()
                    .is_some_and(|model| model.connection_id == connection.config.id)
                    && default_model
                        .as_ref()
                        .is_some_and(|model| model.model_id.trim().is_empty())
                {
                    ConnectionReadiness::NeedsModel
                } else {
                    readiness
                },
                default_model: default_model
                    .as_ref()
                    .filter(|model| model.connection_id == connection.config.id)
                    .cloned(),
                issue,
            }
        })
        .collect();
    ConnectionInventory {
        mode: loaded.mode,
        default_model,
        entries,
        issues: loaded.issues,
    }
}

/// Builds a secret-free inventory and verifies exact stored-credential references asynchronously.
pub async fn connection_inventory(
    root_config: &RootConfig,
    credential_store: &dyn ProviderCredentialStore,
    environment: &dyn CredentialEnvironment,
) -> ConnectionInventory {
    static NEVER_CANCELLED: AtomicBool = AtomicBool::new(false);
    connection_inventory_with_cancellation(
        root_config,
        credential_store,
        environment,
        &NEVER_CANCELLED,
    )
    .await
}

/// Builds a verified inventory while allowing an owning product surface to retire stale work.
pub async fn connection_inventory_with_cancellation(
    root_config: &RootConfig,
    credential_store: &dyn ProviderCredentialStore,
    environment: &dyn CredentialEnvironment,
    cancelled: &AtomicBool,
) -> ConnectionInventory {
    let mut inventory = connection_inventory_offline(root_config, environment);
    let loaded = load_provider_connections(root_config);
    for entry in &mut inventory.entries {
        if cancelled.load(Ordering::Acquire) {
            break;
        }
        let Some(connection) = loaded.connections.get(&entry.id) else {
            continue;
        };
        let LoadedCredentialRef::Config(
            CredentialRefConfig::SystemKeyring { id } | CredentialRefConfig::Stored { id },
        ) = &connection.credential
        else {
            continue;
        };
        match credential_store.load(id).await {
            Ok(Some(record))
                if record.credential_id == *id
                    && record.provider_family == connection.config.provider =>
            {
                entry.readiness = ConnectionReadiness::Ready;
                entry.issue = None;
            }
            Ok(Some(_)) => {
                entry.readiness = ConnectionReadiness::CredentialUnavailable;
                entry.issue = Some(ConnectionIssueView {
                    code: ProviderCredentialErrorCode::CredentialRecordMismatch
                        .as_str()
                        .to_owned(),
                    message: "credential record does not match this connection".to_owned(),
                });
            }
            Ok(None) => {
                entry.readiness = ConnectionReadiness::NeedsCredential;
                entry.issue = Some(ConnectionIssueView {
                    code: ProviderCredentialErrorCode::CredentialMissing
                        .as_str()
                        .to_owned(),
                    message: "credential record is missing".to_owned(),
                });
            }
            Err(error) => {
                entry.readiness = ConnectionReadiness::CredentialUnavailable;
                entry.issue = Some(ConnectionIssueView {
                    code: error.code.to_owned(),
                    message: "credential store is unavailable".to_owned(),
                });
            }
        }
        if cancelled.load(Ordering::Acquire) {
            break;
        }
    }
    inventory
}

/// Builds a stored-credential verified inventory from synchronous product surfaces.
///
/// The verification owns a dedicated thread and current-thread runtime so callers are safe inside
/// either Tokio runtime flavor and credential-store work never blocks an async executor thread.
#[must_use]
pub fn connection_inventory_native(root_config: &RootConfig) -> ConnectionInventory {
    let root_config = root_config.clone();
    let fallback = root_config.clone();
    std::thread::Builder::new()
        .name("sigil-credential-doctor".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()?;
            let inventory = runtime.block_on(connection_inventory(
                &root_config,
                &super::ConfiguredProviderCredentialStore::from_root_config(&root_config),
                &super::ProcessCredentialEnvironment,
            ));
            runtime.shutdown_timeout(std::time::Duration::from_millis(100));
            Some(inventory)
        })
        .ok()
        .and_then(|thread| thread.join().ok())
        .flatten()
        .unwrap_or_else(|| {
            connection_inventory_offline(&fallback, &super::ProcessCredentialEnvironment)
        })
}

fn offline_credential_state(
    credential: &LoadedCredentialRef,
    environment: &dyn CredentialEnvironment,
) -> (
    CredentialSourceView,
    ConnectionReadiness,
    Option<ConnectionIssueView>,
) {
    match credential {
        LoadedCredentialRef::LegacyInline(_) => (
            CredentialSourceView::LegacyPlaintext,
            ConnectionReadiness::Unverified,
            Some(ConnectionIssueView {
                code: "legacy_plaintext_migration_required".to_owned(),
                message: "legacy plaintext credential requires migration".to_owned(),
            }),
        ),
        LoadedCredentialRef::Config(CredentialRefConfig::Environment { name }) => {
            if environment.read(name).is_some() {
                (
                    CredentialSourceView::Environment,
                    ConnectionReadiness::Ready,
                    None,
                )
            } else {
                (
                    CredentialSourceView::Environment,
                    ConnectionReadiness::NeedsCredential,
                    Some(ConnectionIssueView {
                        code: ProviderCredentialErrorCode::CredentialMissing
                            .as_str()
                            .to_owned(),
                        message: "configured credential environment value is missing".to_owned(),
                    }),
                )
            }
        }
        LoadedCredentialRef::Config(CredentialRefConfig::SystemKeyring { .. }) => (
            CredentialSourceView::SystemKeyring,
            ConnectionReadiness::Unverified,
            Some(ConnectionIssueView {
                code: "credential_unverified".to_owned(),
                message: "native credential is not checked by offline inventory".to_owned(),
            }),
        ),
        LoadedCredentialRef::Config(CredentialRefConfig::Stored { .. }) => (
            CredentialSourceView::Stored,
            ConnectionReadiness::Unverified,
            Some(ConnectionIssueView {
                code: "credential_unverified".to_owned(),
                message: "stored credential is not checked by offline inventory".to_owned(),
            }),
        ),
        LoadedCredentialRef::Config(CredentialRefConfig::None) => {
            (CredentialSourceView::None, ConnectionReadiness::Ready, None)
        }
    }
}

fn endpoint_display(base_url: &str) -> String {
    let Ok(url) = url::Url::parse(base_url) else {
        return "invalid endpoint".to_owned();
    };
    if url
        .host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case("localhost"))
        || url.host().is_some_and(|host| match host {
            url::Host::Ipv4(address) => address.is_loopback(),
            url::Host::Ipv6(address) => address.is_loopback(),
            url::Host::Domain(_) => false,
        })
    {
        "local loopback endpoint".to_owned()
    } else if url.scheme() == "https" {
        "configured HTTPS endpoint".to_owned()
    } else {
        "configured custom endpoint".to_owned()
    }
}
