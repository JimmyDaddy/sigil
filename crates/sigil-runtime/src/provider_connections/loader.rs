use std::collections::BTreeMap;

use anyhow::Result;
use sigil_kernel::{CONFIG_VERSION_V2, ConnectionId, ModelRef, RootConfig};

use super::{
    ConfigMode, ConnectionConfigIssue, LoadedConnection, LoadedCredentialRef,
    LoadedProviderConnections, ProviderConnectionConfig,
};

pub fn load_provider_connections(root_config: &RootConfig) -> LoadedProviderConnections {
    if root_config.config_version != CONFIG_VERSION_V2 {
        return LoadedProviderConnections {
            mode: ConfigMode::Invalid,
            default_model: None,
            connections: BTreeMap::new(),
            issues: vec![ConnectionConfigIssue {
                connection_id: None,
                code: "invalid_config_version",
                message: format!("config_version = {CONFIG_VERSION_V2} is required"),
            }],
        };
    }

    let mut connections = BTreeMap::new();
    let mut issues = Vec::new();
    for (raw_id, value) in &root_config.connections {
        let id = match ConnectionId::new(raw_id.clone()) {
            Ok(id) => id,
            Err(error) => {
                issues.push(ConnectionConfigIssue {
                    connection_id: Some(safe_issue_identity(raw_id)),
                    code: "invalid_connection_id",
                    message: error.to_string(),
                });
                continue;
            }
        };
        match ProviderConnectionConfig::from_raw(id.clone(), value.clone()) {
            Ok(config) => {
                let credential = LoadedCredentialRef::Config(config.credential.clone());
                connections.insert(id, LoadedConnection { config, credential });
            }
            Err(error) => issues.push(ConnectionConfigIssue {
                connection_id: Some(id.to_string()),
                code: "invalid_connection",
                message: format!("{error:#}"),
            }),
        }
    }

    let default_model = match root_config.agent.connection.clone() {
        Some(connection_id) => {
            match ModelRef::new(connection_id.clone(), root_config.agent.model.clone()) {
                Ok(model_ref) if connections.contains_key(&connection_id) => Some(model_ref),
                Ok(_) => {
                    issues.push(ConnectionConfigIssue {
                        connection_id: Some(connection_id.to_string()),
                        code: "connection_not_found",
                        message: "default model references a missing or invalid connection"
                            .to_owned(),
                    });
                    None
                }
                Err(error) => {
                    issues.push(ConnectionConfigIssue {
                        connection_id: Some(connection_id.to_string()),
                        code: "invalid_model_id",
                        message: error.to_string(),
                    });
                    None
                }
            }
        }
        None => {
            issues.push(ConnectionConfigIssue {
                connection_id: None,
                code: "model_route_not_configured",
                message: "agent.connection is required".to_owned(),
            });
            None
        }
    };

    LoadedProviderConnections {
        mode: ConfigMode::V2,
        default_model,
        connections,
        issues,
    }
}

pub fn materialize_root_config(
    root_config: &RootConfig,
    connections: &BTreeMap<ConnectionId, ProviderConnectionConfig>,
    default_model: &ModelRef,
) -> Result<RootConfig> {
    anyhow::ensure!(
        connections.contains_key(&default_model.connection_id),
        "default model references a missing connection"
    );
    let mut raw_connections = BTreeMap::new();
    for (id, connection) in connections {
        anyhow::ensure!(
            id == &connection.id,
            "connection registry key does not match connection identity"
        );
        raw_connections.insert(id.to_string(), connection.to_raw()?);
    }

    let mut next = root_config.clone();
    next.config_version = CONFIG_VERSION_V2;
    next.agent.connection = Some(default_model.connection_id.clone());
    next.agent.model = default_model.model_id.clone();
    next.connections = raw_connections;
    Ok(next)
}

fn safe_issue_identity(raw: &str) -> String {
    use sha2::{Digest, Sha256};

    let digest = Sha256::digest(raw.as_bytes());
    let short_hash = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("!invalid-{short_hash}")
}
