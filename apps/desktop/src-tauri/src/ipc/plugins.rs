//! Path-free plugin declarations for explicit session-bound trust review.

use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopPluginCatalog {
    pub(crate) plugins: Vec<DesktopPluginReview>,
    pub(crate) warning_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopPluginReview {
    pub(crate) plugin_id: String,
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) manifest_hash: String,
    pub(crate) capability_digest: String,
    pub(crate) trust: String,
    pub(crate) process_cleanup: Option<sigil_desktop::DesktopPluginCleanupStatus>,
    pub(crate) capabilities: Vec<DesktopPluginCapabilityView>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopPluginCapabilityView {
    pub(crate) kind: String,
    pub(crate) label: String,
    pub(crate) approval: Option<String>,
    pub(crate) allow_secrets: bool,
    pub(crate) egress_logging: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopPluginReviewReceipt {
    pub(crate) plugin_id: String,
    pub(crate) enabled: bool,
    pub(crate) process_cleanup: Option<sigil_desktop::DesktopPluginCleanupStatus>,
}

impl From<sigil_desktop::DesktopPluginCatalog> for DesktopPluginCatalog {
    fn from(value: sigil_desktop::DesktopPluginCatalog) -> Self {
        Self {
            warning_count: value.warning_count,
            plugins: value
                .plugins
                .into_iter()
                .map(|plugin| DesktopPluginReview {
                    plugin_id: plugin.plugin_id,
                    name: plugin.name,
                    version: plugin.version,
                    manifest_hash: plugin.manifest_hash,
                    capability_digest: plugin.capability_digest,
                    trust: plugin.trust,
                    process_cleanup: plugin.process_cleanup,
                    capabilities: plugin
                        .capabilities
                        .into_iter()
                        .map(|cap| DesktopPluginCapabilityView {
                            kind: cap.kind,
                            label: cap.label,
                            approval: cap.approval,
                            allow_secrets: cap.allow_secrets,
                            egress_logging: cap.egress_logging,
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}
impl From<sigil_desktop::DesktopPluginReviewReceipt> for DesktopPluginReviewReceipt {
    fn from(value: sigil_desktop::DesktopPluginReviewReceipt) -> Self {
        Self {
            plugin_id: value.plugin_id,
            enabled: value.enabled,
            process_cleanup: value.process_cleanup,
        }
    }
}

#[cfg(test)]
#[path = "tests/plugins_tests.rs"]
mod tests;
