use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

const SERVER_INFO_SCHEMA_VERSION: u16 = 4;
const HTTP_PROTOCOL_VERSION: u16 = 2;

/// Authentication mode required by the desktop runtime bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopServerAuthentication {
    /// A private, per-launch bearer injected outside argv and response payloads.
    Bearer,
}

/// Frozen coarse capabilities required by the first desktop shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopServerCapabilities {
    /// Historical workspace sessions can be listed.
    pub session_catalog: bool,
    /// Historical candidates can be truth-validated and reopened.
    pub durable_session_reopen: bool,
    /// Bound durable sessions expose safe bounded transcript pages.
    pub bounded_transcript_replay: bool,
    /// Durable events can be replayed with a bound cursor.
    pub durable_event_replay: bool,
    /// Live events can be followed while the child is active.
    pub live_events: bool,
    /// Pending approvals can be resolved.
    pub approval: bool,
    /// Active runs can be cooperatively cancelled.
    pub cancellation: bool,
    /// Task verification recommendation and exact rerun are available.
    pub verification: bool,
    /// Typed model, permission-mode, and context usage facts are available.
    pub run_context: bool,
}

impl DesktopServerCapabilities {
    fn supports_desktop_v1(&self) -> bool {
        self.session_catalog
            && self.durable_session_reopen
            && self.bounded_transcript_replay
            && self.durable_event_replay
            && self.live_events
            && self.approval
            && self.cancellation
            && self.verification
            && self.run_context
    }
}

/// Secret-free metadata accepted from both startup stdout and `/server-info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopServerInfo {
    /// Version of this metadata object.
    pub schema_version: u16,
    /// Stable command/event protocol version.
    pub protocol_version: u16,
    /// Version of the child binary.
    pub server_version: String,
    /// Stable identifier for the owned workspace.
    pub workspace_id: String,
    /// Actual loopback listener address.
    pub bind_addr: String,
    /// Authentication enforced by non-health routes.
    pub authentication: DesktopServerAuthentication,
    /// Whether dropping the owner pipe starts graceful shutdown.
    pub shutdown_on_stdin_close: bool,
    /// Coarse desktop feature support.
    pub capabilities: DesktopServerCapabilities,
}

impl DesktopServerInfo {
    pub(crate) fn validate(&self) -> Result<SocketAddr, &'static str> {
        if self.schema_version != SERVER_INFO_SCHEMA_VERSION {
            return Err("unsupported server-info schema version");
        }
        if self.protocol_version != HTTP_PROTOCOL_VERSION {
            return Err("unsupported HTTP protocol version");
        }
        if self.server_version.is_empty()
            || self.server_version.len() > 128
            || !self
                .server_version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-'))
        {
            return Err("invalid server version metadata");
        }
        if self.workspace_id.is_empty()
            || self.workspace_id.len() > 512
            || !self
                .workspace_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err("invalid workspace identity metadata");
        }
        if !self.shutdown_on_stdin_close {
            return Err("owner-channel shutdown is unavailable");
        }
        if !self.capabilities.supports_desktop_v1() {
            return Err("required desktop capability is unavailable");
        }
        let address = self
            .bind_addr
            .parse::<SocketAddr>()
            .map_err(|_| "invalid listener address")?;
        if !address.ip().is_loopback() {
            return Err("desktop listener is not loopback");
        }
        Ok(address)
    }
}

#[cfg(test)]
#[path = "tests/protocol_tests.rs"]
mod tests;
