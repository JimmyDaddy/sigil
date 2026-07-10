use thiserror::Error;

use super::framing::McpFramingError;
use sigil_kernel::{ToolErrorKind, ToolResult};

#[derive(Debug, Clone)]
pub(super) enum McpTerminalCause {
    StderrLimit { total_bytes: u64, limit_bytes: u64 },
    StderrReaderFailed { total_bytes: u64, reason: String },
}

impl McpTerminalCause {
    fn code(&self) -> &'static str {
        match self {
            Self::StderrLimit { .. } => "stderr_limit",
            Self::StderrReaderFailed { .. } => "stderr_reader_failed",
        }
    }

    fn total_bytes(&self) -> u64 {
        match self {
            Self::StderrLimit { total_bytes, .. }
            | Self::StderrReaderFailed { total_bytes, .. } => *total_bytes,
        }
    }

    fn limit_bytes(&self) -> Option<u64> {
        match self {
            Self::StderrLimit { limit_bytes, .. } => Some(*limit_bytes),
            Self::StderrReaderFailed { .. } => None,
        }
    }

    fn reader_reason(&self) -> Option<&str> {
        match self {
            Self::StderrReaderFailed { reason, .. } => Some(reason),
            Self::StderrLimit { .. } => None,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct McpCleanupEvidence {
    pub(super) completed: bool,
    pub(super) reason: String,
}

impl McpCleanupEvidence {
    pub(super) fn summary(&self) -> String {
        format!(
            "cleanup_completed={}, cleanup_reason={}",
            self.completed, self.reason
        )
    }
}

#[derive(Debug, Error)]
pub(super) enum McpClientError {
    #[error("MCP operation {operation} timed out after {timeout_ms} ms")]
    Timeout { operation: String, timeout_ms: u64 },
    #[error("MCP operation {operation} failed while decoding stdio framing")]
    Framing {
        operation: String,
        #[source]
        source: McpFramingError,
    },
    #[error("MCP connection for {server_name} is closed: {reason}")]
    ConnectionClosed {
        server_name: String,
        reason: String,
        cause: Option<McpTerminalCause>,
    },
    #[error(
        "MCP operation {operation} received unexpected response id ({observed_type}: {observed_preview})"
    )]
    UnexpectedResponseId {
        operation: String,
        observed_type: &'static str,
        observed_preview: String,
    },
    #[error("MCP operation {operation} received an invalid JSON-RPC envelope: {reason}")]
    InvalidEnvelope { operation: String, reason: String },
    #[error(
        "MCP operation {operation} reached its limit of {limit} inbound messages before a response"
    )]
    MessageLimit {
        operation: String,
        limit: usize,
        observed_at_least: usize,
    },
    #[error("MCP operation {operation} exceeded {limit_bytes} cumulative response bytes")]
    CumulativeBytesLimit {
        operation: String,
        limit_bytes: usize,
        observed_at_least_bytes: usize,
    },
    #[error("MCP request id space is exhausted")]
    RequestIdExhausted,
    #[error("MCP operation {operation} failed while handling an inbound message")]
    Inbound {
        operation: String,
        #[source]
        source: anyhow::Error,
    },
    #[error("{source}; MCP transport cleanup: {cleanup_reason}")]
    WithCleanup {
        #[source]
        source: Box<McpClientError>,
        cleanup_completed: bool,
        cleanup_reason: String,
    },
}

impl McpClientError {
    pub(super) fn code(&self) -> &'static str {
        match self {
            Self::Timeout { .. } => "timeout",
            Self::Framing { source, .. } => source.code(),
            Self::ConnectionClosed { cause, .. } => cause
                .as_ref()
                .map_or("connection_closed", McpTerminalCause::code),
            Self::UnexpectedResponseId { .. } => "unexpected_response_id",
            Self::InvalidEnvelope { .. } => "invalid_jsonrpc_envelope",
            Self::MessageLimit { .. } => "message_limit",
            Self::CumulativeBytesLimit { .. } => "cumulative_bytes_limit",
            Self::RequestIdExhausted => "request_id_exhausted",
            Self::Inbound { .. } => "inbound_message_failed",
            Self::WithCleanup { source, .. } => source.code(),
        }
    }

    pub(super) fn timeout_ms(&self) -> Option<u64> {
        match self {
            Self::Timeout { timeout_ms, .. } => Some(*timeout_ms),
            Self::WithCleanup { source, .. } => source.timeout_ms(),
            _ => None,
        }
    }

    pub(super) fn is_timeout(&self) -> bool {
        match self {
            Self::Timeout { .. } => true,
            Self::WithCleanup { source, .. } => source.is_timeout(),
            _ => false,
        }
    }

    fn is_resource_limit(&self) -> bool {
        match self {
            Self::Framing {
                source: McpFramingError::FrameTooLarge { .. },
                ..
            }
            | Self::MessageLimit { .. }
            | Self::CumulativeBytesLimit { .. }
            | Self::ConnectionClosed {
                cause: Some(McpTerminalCause::StderrLimit { .. }),
                ..
            } => true,
            Self::WithCleanup { source, .. } => source.is_resource_limit(),
            _ => false,
        }
    }

    fn resource_limit_details(
        &self,
    ) -> (Option<usize>, Option<usize>, Option<usize>, Option<usize>) {
        match self {
            Self::Framing {
                source:
                    McpFramingError::FrameTooLarge {
                        limit_bytes,
                        observed_at_least_bytes,
                    },
                ..
            } => (
                None,
                None,
                Some(*limit_bytes),
                Some(*observed_at_least_bytes),
            ),
            Self::MessageLimit {
                limit,
                observed_at_least,
                ..
            } => (Some(*limit), Some(*observed_at_least), None, None),
            Self::CumulativeBytesLimit {
                limit_bytes,
                observed_at_least_bytes,
                ..
            } => (
                None,
                None,
                Some(*limit_bytes),
                Some(*observed_at_least_bytes),
            ),
            Self::ConnectionClosed {
                cause:
                    Some(McpTerminalCause::StderrLimit {
                        total_bytes,
                        limit_bytes,
                    }),
                ..
            } => (
                None,
                None,
                usize::try_from(*limit_bytes).ok(),
                usize::try_from(*total_bytes).ok(),
            ),
            Self::WithCleanup { source, .. } => source.resource_limit_details(),
            _ => (None, None, None, None),
        }
    }

    pub(super) fn with_cleanup(self, cleanup: McpCleanupEvidence) -> Self {
        if matches!(self, Self::WithCleanup { .. }) {
            return self;
        }
        Self::WithCleanup {
            source: Box::new(self),
            cleanup_completed: cleanup.completed,
            cleanup_reason: cleanup.reason,
        }
    }

    fn cleanup(&self) -> Option<(bool, &str)> {
        match self {
            Self::WithCleanup {
                cleanup_completed,
                cleanup_reason,
                ..
            } => Some((*cleanup_completed, cleanup_reason)),
            _ => None,
        }
    }

    fn terminal_cause(&self) -> Option<&McpTerminalCause> {
        match self {
            Self::ConnectionClosed { cause, .. } => cause.as_ref(),
            Self::WithCleanup { source, .. } => source.terminal_cause(),
            _ => None,
        }
    }

    pub(super) fn to_tool_result(
        &self,
        call_id: impl Into<String>,
        tool_name: impl Into<String>,
        server_name: &str,
    ) -> ToolResult {
        let kind = if self.is_timeout() {
            ToolErrorKind::Timeout
        } else if self.is_resource_limit() {
            ToolErrorKind::ResourceLimit
        } else {
            ToolErrorKind::Protocol
        };
        let cleanup = self.cleanup();
        let terminal_cause = self.terminal_cause();
        let (limit, observed_at_least, limit_bytes, observed_at_least_bytes) =
            self.resource_limit_details();
        ToolResult::error(call_id, tool_name, kind, self.to_string()).with_error_details(
            false,
            serde_json::json!({
                "mcp": {
                    "server": server_name,
                    "code": self.code(),
                    "timeout_ms": self.timeout_ms(),
                    "connection_state": "closed",
                    "retry_action": "refresh_mcp_server",
                    "cleanup_completed": cleanup.map(|(completed, _)| completed),
                    "cleanup_reason": cleanup.map(|(_, reason)| reason),
                    "terminal_cause": terminal_cause.map(McpTerminalCause::code),
                    "stderr_total_bytes": terminal_cause.map(McpTerminalCause::total_bytes),
                    "stderr_limit_bytes": terminal_cause.and_then(McpTerminalCause::limit_bytes),
                    "stderr_reader_cause": terminal_cause.and_then(McpTerminalCause::reader_reason),
                    "limit": limit,
                    "observed_at_least": observed_at_least,
                    "limit_bytes": limit_bytes,
                    "observed_at_least_bytes": observed_at_least_bytes,
                }
            }),
        )
    }
}
