//! Path-free plugin declarations for explicit session-bound trust review.

use serde::{Deserialize, Serialize};
use sigil_kernel::{PluginCapability, safe_persistence_text};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpPluginCatalog {
    pub plugins: Vec<HttpPluginReview>,
    pub warning_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpPluginReview {
    pub plugin_id: String,
    pub name: String,
    pub version: String,
    pub manifest_hash: String,
    pub capability_digest: String,
    pub trust: String,
    /// Canonical cleanup of prior generations, independent from current plugin trust.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_cleanup: Option<sigil_application::PluginCleanupStatus>,
    pub capabilities: Vec<HttpPluginCapabilityView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpPluginCapabilityView {
    pub kind: String,
    pub label: String,
    pub approval: Option<String>,
    pub allow_secrets: bool,
    pub egress_logging: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpPluginReviewReceipt {
    pub plugin_id: String,
    pub enabled: bool,
    /// Cleanup observed for this operation; does not clear older catalog warnings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_cleanup: Option<sigil_application::PluginCleanupStatus>,
}

impl TryFrom<sigil_runtime::plugin_management::ApplicationPluginCatalog> for HttpPluginCatalog {
    type Error = anyhow::Error;

    fn try_from(
        catalog: sigil_runtime::plugin_management::ApplicationPluginCatalog,
    ) -> Result<Self, Self::Error> {
        let mut plugins = Vec::with_capacity(catalog.manifests.len());
        for manifest in catalog.manifests {
            let process_cleanup = catalog
                .process_cleanup
                .get(&manifest.plugin_id)
                .map(|status| match status {
                    sigil_kernel::PluginCleanupStatus::Confirmed => {
                        sigil_application::PluginCleanupStatus::Confirmed
                    }
                    sigil_kernel::PluginCleanupStatus::Unknown => {
                        sigil_application::PluginCleanupStatus::Unknown
                    }
                    sigil_kernel::PluginCleanupStatus::Unconfirmed => {
                        sigil_application::PluginCleanupStatus::Unconfirmed
                    }
                });
            let capability_digest = manifest.capability_digest()?;
            let capabilities = manifest
                .capabilities
                .iter()
                .map(|capability| {
                    let (kind, label, approval, allow_secrets, egress_logging) = match capability {
                        PluginCapability::Agent { .. } => {
                            ("agent", "Agent definition".to_owned(), None, false, false)
                        }
                        PluginCapability::Skill { .. } => {
                            ("skill", "Skill instructions".to_owned(), None, false, false)
                        }
                        PluginCapability::Hook {
                            id,
                            approval,
                            allow_secrets,
                            egress_logging,
                            ..
                        } => (
                            "hook",
                            safe_label(id),
                            Some(approval.as_str().to_owned()),
                            *allow_secrets,
                            *egress_logging,
                        ),
                        PluginCapability::McpServer {
                            name,
                            approval,
                            allow_secrets,
                            egress_logging,
                            ..
                        } => (
                            "mcp",
                            safe_label(name),
                            Some(approval.as_str().to_owned()),
                            *allow_secrets,
                            *egress_logging,
                        ),
                    };
                    HttpPluginCapabilityView {
                        kind: kind.to_owned(),
                        label,
                        approval,
                        allow_secrets,
                        egress_logging,
                    }
                })
                .collect();
            plugins.push(HttpPluginReview {
                plugin_id: manifest.plugin_id,
                name: safe_label(&manifest.name),
                version: safe_label(&manifest.version),
                manifest_hash: manifest.manifest_hash,
                capability_digest,
                trust: manifest.trust.as_str().to_owned(),
                process_cleanup,
                capabilities,
            });
        }
        Ok(Self {
            plugins,
            warning_count: catalog.warnings.len(),
        })
    }
}

fn safe_label(value: &str) -> String {
    safe_persistence_text(value).chars().take(256).collect()
}
