use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use futures::{Stream, StreamExt};

use crate::SecretRedactor;

/// Maximum number of bytes retained from a non-success provider response body.
pub const PROVIDER_ERROR_BODY_LIMIT_BYTES: usize = 16 * 1024;

/// Bounded and redacted text captured from a non-success provider response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderErrorBody {
    text: String,
    captured_bytes: usize,
    truncated: bool,
}

impl ProviderErrorBody {
    /// Returns the safe body text that may be used for classification or diagnostics.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the number of raw response bytes retained before decoding and redaction.
    #[must_use]
    pub fn captured_bytes(&self) -> usize {
        self.captured_bytes
    }

    /// Reports whether collection stopped at the configured byte limit.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

/// Reads a transport-neutral byte stream into a bounded, timed, and redacted error body.
///
/// Collection stops as soon as `max_bytes` is reached. The returned text is bounded again after
/// lossy UTF-8 decoding and redaction so replacements cannot grow the diagnostic past the hard
/// limit.
///
/// # Errors
///
/// Returns an error when the body stream fails or does not finish before `timeout_duration`.
pub async fn read_provider_error_body<S, B, E>(
    mut stream: S,
    timeout_duration: Duration,
    max_bytes: usize,
    redactor: &SecretRedactor,
) -> Result<ProviderErrorBody>
where
    S: Stream<Item = std::result::Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
    E: std::error::Error + Send + Sync + 'static,
{
    let read = async {
        let mut body = Vec::with_capacity(max_bytes.min(4096));
        let mut truncated = false;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("failed to read provider error response body chunk")?;
            let bytes = chunk.as_ref();
            let remaining = max_bytes.saturating_sub(body.len());
            if bytes.len() >= remaining {
                body.extend_from_slice(&bytes[..remaining]);
                truncated = true;
                break;
            }
            body.extend_from_slice(bytes);
        }

        let captured_bytes = body.len();
        let redacted = if truncated {
            redactor.redact_truncated_bytes(&body)
        } else {
            redactor.redact_text(&String::from_utf8_lossy(&body))
        };
        let text = truncate_utf8_bytes(redacted, max_bytes);
        Ok(ProviderErrorBody {
            text,
            captured_bytes,
            truncated,
        })
    };

    tokio::time::timeout(timeout_duration, read)
        .await
        .map_err(|_| {
            anyhow!(
                "provider error response body timed out after {} ms",
                duration_millis_u64(timeout_duration)
            )
        })?
}

fn truncate_utf8_bytes(mut text: String, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text;
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    text.truncate(boundary);
    text
}

fn duration_millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "tests/provider_error_tests.rs"]
mod tests;
