use sigil_kernel::{NetworkPolicy, RootConfig, WebSearchFailureClass};

use super::{DoctorReport, DoctorStatus};

/// Offline summary of the provider-hosted web-search capability selected for a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebDoctorHostedCapability {
    Supported,
    Unsupported,
    Unknown,
}

impl WebDoctorHostedCapability {
    fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Unsupported => "unsupported",
            Self::Unknown => "unknown",
        }
    }
}

/// Offline state of an exact configured stable-MCP search binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebDoctorBindingState {
    Absent,
    PresentUnresolved,
    EligibleKnownVersioned,
    EligibleGenericQueryText,
    Unavailable(WebSearchFailureClass),
}

impl WebDoctorBindingState {
    fn as_str(self) -> String {
        match self {
            Self::Absent => "absent".to_owned(),
            Self::PresentUnresolved => "present_unresolved".to_owned(),
            Self::EligibleKnownVersioned => "eligible_known_versioned".to_owned(),
            Self::EligibleGenericQueryText => "eligible_generic_query_text".to_owned(),
            Self::Unavailable(failure) => format!("unavailable:{failure:?}").to_lowercase(),
        }
    }
}

/// Secret-safe, network-free input for rendering Web V1 doctor checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebDoctorSnapshot {
    pub network_policy: NetworkPolicy,
    pub hosted_capability: WebDoctorHostedCapability,
    pub binding: WebDoctorBindingState,
    pub bundled_enabled: bool,
    pub bundled_profile_id: String,
    pub bundled_safe_destination: String,
    pub bundled_codec: String,
    pub public_route_enabled: bool,
}

impl WebDoctorSnapshot {
    /// Returns the internal-only Web V1 state used before the atomic public cutover.
    #[must_use]
    pub fn internal_only() -> Self {
        Self {
            network_policy: NetworkPolicy::Allow,
            hosted_capability: WebDoctorHostedCapability::Unknown,
            binding: WebDoctorBindingState::Absent,
            bundled_enabled: false,
            bundled_profile_id: "builtin:exa-anonymous".to_owned(),
            bundled_safe_destination: "https://mcp.exa.ai/mcp".to_owned(),
            bundled_codec: "exa_text_v1".to_owned(),
            public_route_enabled: false,
        }
    }

    /// Builds the public, network-free projection from the parsed root policy.
    #[must_use]
    pub fn from_root_config(root: &RootConfig) -> Self {
        Self {
            network_policy: root.web.network_mode,
            hosted_capability: WebDoctorHostedCapability::Unknown,
            binding: if root.web.search_mcp.is_some() {
                WebDoctorBindingState::PresentUnresolved
            } else {
                WebDoctorBindingState::Absent
            },
            bundled_enabled: root.web.enabled && root.web.bundled_search.enabled,
            bundled_profile_id: "builtin:exa-anonymous".to_owned(),
            bundled_safe_destination: "https://mcp.exa.ai/".to_owned(),
            bundled_codec: "exa_text_v1".to_owned(),
            public_route_enabled: root.web.enabled,
        }
    }
}

/// Adds network-free Web V1 diagnostics without probing a provider, MCP server, or bundled route.
pub fn append_web_doctor_snapshot(report: &mut DoctorReport, snapshot: &WebDoctorSnapshot) {
    let binding = snapshot.binding.as_str();
    report.push(
        if snapshot.public_route_enabled {
            DoctorStatus::Ok
        } else {
            DoctorStatus::Warn
        },
        "web:route",
        format!(
            "public_route={} network_policy={:?} hosted={} binding={binding}",
            if snapshot.public_route_enabled {
                "enabled"
            } else {
                "internal_only"
            },
            snapshot.network_policy,
            snapshot.hosted_capability.as_str(),
        ),
    );
    report.push(
        DoctorStatus::Ok,
        "web:bundled",
        format!(
            "enabled={} profile={} destination={} codec={} state=unprobed network=offline",
            snapshot.bundled_enabled,
            snapshot.bundled_profile_id,
            snapshot.bundled_safe_destination,
            snapshot.bundled_codec,
        ),
    );
    if matches!(snapshot.binding, WebDoctorBindingState::Unavailable(_)) {
        report.push_with_remediation(
            DoctorStatus::Warn,
            "web:binding",
            format!("configured stable binding {binding}; raw MCP tool remains separate"),
            Some("repair the configured server/tool or use an explicitly selected route"),
        );
    }
}
