use std::collections::BTreeMap;

use sigil_kernel::{ConnectionId, ModelRef};

use super::{LoadedCredentialRef, ProviderConnectionConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigMode {
    LegacyV1,
    V2,
    Mixed,
    UnsupportedFuture,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionConfigIssue {
    pub connection_id: Option<String>,
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct LoadedConnection {
    pub config: ProviderConnectionConfig,
    pub credential: LoadedCredentialRef,
}

#[derive(Debug, Clone)]
pub struct LoadedProviderConnections {
    pub mode: ConfigMode,
    pub default_model: Option<ModelRef>,
    pub connections: BTreeMap<ConnectionId, LoadedConnection>,
    pub issues: Vec<ConnectionConfigIssue>,
}

impl LoadedProviderConnections {
    #[must_use]
    pub fn migration_required(&self) -> bool {
        self.mode == ConfigMode::LegacyV1
            && self.connections.values().any(|connection| {
                matches!(connection.credential, LoadedCredentialRef::LegacyInline(_))
            })
    }

    pub fn default_connection(&self) -> anyhow::Result<&LoadedConnection> {
        let model_ref = self
            .default_model
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("model_route_not_configured"))?;
        self.connections
            .get(&model_ref.connection_id)
            .ok_or_else(|| anyhow::anyhow!("connection_not_found"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionInventory {
    pub mode: ConfigMode,
    pub default_model: Option<ModelRef>,
    pub entries: Vec<ConnectionInventoryEntry>,
    pub issues: Vec<ConnectionConfigIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionInventoryEntry {
    pub id: ConnectionId,
    pub label: String,
    pub provider_label: String,
    pub protocol_label: String,
    pub endpoint_display: String,
    pub credential_source: CredentialSourceView,
    pub readiness: ConnectionReadiness,
    pub default_model: Option<ModelRef>,
    pub issue: Option<ConnectionIssueView>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialSourceView {
    Environment,
    SystemKeyring,
    Stored,
    None,
    LegacyPlaintext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionReadiness {
    Ready,
    NeedsCredential,
    CredentialUnavailable,
    NeedsModel,
    Unverified,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionIssueView {
    pub code: String,
    pub message: String,
}
