//! Path-free plugin declarations for explicit session-bound trust review.

use serde::{Deserialize, Serialize};

/// Host-observed cleanup of prior plugin processes, separate from permission to start new ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopPluginCleanupStatus {
    /// The host confirmed the relevant earlier processes stopped.
    Confirmed,
    /// Available evidence cannot establish the earlier processes' state.
    Unknown,
    /// The host could not confirm the requested cleanup completed.
    Unconfirmed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopPluginCatalog {
    pub plugins: Vec<DesktopPluginReview>,
    pub warning_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopPluginReview {
    pub plugin_id: String,
    pub name: String,
    pub version: String,
    pub manifest_hash: String,
    pub capability_digest: String,
    pub trust: String,
    /// Canonical status of earlier cleanup, including unresolved work after re-enabling trust.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_cleanup: Option<DesktopPluginCleanupStatus>,
    pub capabilities: Vec<DesktopPluginCapabilityView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopPluginCapabilityView {
    pub kind: String,
    pub label: String,
    pub approval: Option<String>,
    pub allow_secrets: bool,
    pub egress_logging: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopPluginReviewReceipt {
    pub plugin_id: String,
    pub enabled: bool,
    /// This operation's cleanup result; the plugin catalog remains the canonical history view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_cleanup: Option<DesktopPluginCleanupStatus>,
}
