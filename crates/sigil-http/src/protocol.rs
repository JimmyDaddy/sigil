use serde::{Deserialize, Serialize};
use thiserror::Error as ThisError;

/// Current protocol command/event surface version.
pub const HTTP_PROTOCOL_VERSION: u16 = 2;

/// Versioned command envelope shared by future HTTP, IDE, and TUI command bridges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpCommandEnvelope<T> {
    pub protocol_version: u16,
    pub command_id: String,
    pub client_id: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_stream_sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    pub payload: T,
}

impl<T> HttpCommandEnvelope<T> {
    /// Creates a command envelope using the current HTTP protocol version.
    #[must_use]
    pub fn new(
        command_id: impl Into<String>,
        client_id: impl Into<String>,
        session_id: impl Into<String>,
        payload: T,
    ) -> Self {
        Self {
            protocol_version: HTTP_PROTOCOL_VERSION,
            command_id: command_id.into(),
            client_id: client_id.into(),
            session_id: session_id.into(),
            expected_stream_sequence: None,
            correlation_id: None,
            payload,
        }
    }

    /// Adds an optimistic stream-sequence guard for stale-client protection.
    #[must_use]
    pub fn with_expected_stream_sequence(mut self, sequence: u64) -> Self {
        self.expected_stream_sequence = Some(sequence);
        self
    }

    /// Adds a durable-event correlation id.
    #[must_use]
    pub fn with_correlation_id(mut self, correlation_id: impl Into<String>) -> Self {
        self.correlation_id = Some(correlation_id.into());
        self
    }

    /// Fails closed when a client sends an unsupported command envelope version.
    ///
    /// # Errors
    ///
    /// Returns an error when `protocol_version` does not match the current supported version.
    pub fn ensure_supported(&self) -> Result<(), HttpProtocolVersionError> {
        if self.protocol_version != HTTP_PROTOCOL_VERSION {
            return Err(HttpProtocolVersionError::Unsupported {
                supported: HTTP_PROTOCOL_VERSION,
                received: self.protocol_version,
            });
        }
        Ok(())
    }
}

/// Protocol-version errors for command DTOs.
#[derive(Debug, Clone, PartialEq, Eq, ThisError)]
pub enum HttpProtocolVersionError {
    /// Client command uses another protocol version.
    #[error("unsupported http protocol version {received}; supported version is {supported}")]
    Unsupported { supported: u16, received: u16 },
}
