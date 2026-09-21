//! Identity-bound, byte-bounded reads of one immutable conversation message.

use serde::{Deserialize, Serialize};

pub const MAX_MESSAGE_CONTENT_PAGE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageContentQuery {
    pub display_id: String,
    #[serde(default)]
    pub offset: u64,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_version: Option<String>,
}

const fn default_limit() -> usize {
    MAX_MESSAGE_CONTENT_PAGE_BYTES
}

impl MessageContentQuery {
    pub fn validate(&self) -> Result<(), MessageContentError> {
        if self.display_id.is_empty()
            || self.display_id.len() > 256
            || self.display_id.chars().any(char::is_control)
            || !(4..=MAX_MESSAGE_CONTENT_PAGE_BYTES).contains(&self.limit)
            || self.offset > 2 * 1024 * 1024
            || (self.offset != 0 && self.content_version.is_none())
            || self.content_version.as_ref().is_some_and(|version| {
                version.len() != 64 || !version.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        {
            return Err(MessageContentError::InvalidQuery);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageContentPage {
    pub display_id: String,
    pub message_id: String,
    pub content_version: String,
    pub offset: u64,
    pub next_offset: Option<u64>,
    /// Size of the complete safely redacted UTF-8 body, before paging.
    pub total_bytes: u64,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageContentError {
    InvalidQuery,
    NotFound,
    Stale,
    Corrupt,
    Unavailable,
}

impl std::fmt::Display for MessageContentError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidQuery => "message content query is invalid",
            Self::NotFound => "message content is not available in this conversation",
            Self::Stale => "message content identity has changed",
            Self::Corrupt => "message content failed durable validation",
            Self::Unavailable => "message content is temporarily unavailable",
        })
    }
}

impl std::error::Error for MessageContentError {}
