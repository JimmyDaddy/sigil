use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, anyhow};
use futures::{Stream, StreamExt};
use thiserror::Error;

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

/// Provider-neutral metadata for one HTTP 429 response.
///
/// The source error retains the provider-owned diagnostic while this envelope carries only the
/// bounded scheduling signal that runtime admission is allowed to inspect.
#[derive(Debug, Error)]
#[error("{source}")]
pub struct ProviderRateLimitError {
    retry_after_ms: Option<u64>,
    #[source]
    source: anyhow::Error,
}

impl ProviderRateLimitError {
    /// Wraps one provider-owned 429 error and parses an optional HTTP `Retry-After` header.
    #[must_use]
    pub fn new(source: anyhow::Error, retry_after_header: Option<&str>) -> Self {
        Self {
            retry_after_ms: retry_after_header
                .and_then(|value| parse_retry_after_ms(value, SystemTime::now())),
            source,
        }
    }

    /// Returns the provider-requested cooldown duration when the header was valid.
    #[must_use]
    pub fn retry_after_ms(&self) -> Option<u64> {
        self.retry_after_ms
    }
}

/// Provider-neutral admission error for a route that is already cooling down.
#[derive(Debug, Clone, Error)]
#[error(
    "provider route is cooling down; retry after {retry_after_ms} ms (route {route_fingerprint})"
)]
pub struct ProviderRouteCooldownError {
    retry_after_ms: u64,
    route_fingerprint: String,
}

impl ProviderRouteCooldownError {
    /// Builds a typed cooldown rejection from a safe route fingerprint and bounded delay.
    #[must_use]
    pub fn new(retry_after_ms: u64, route_fingerprint: impl Into<String>) -> Self {
        Self {
            retry_after_ms,
            route_fingerprint: route_fingerprint.into(),
        }
    }

    /// Returns the remaining cooldown delay at the time admission was rejected.
    #[must_use]
    pub fn retry_after_ms(&self) -> u64 {
        self.retry_after_ms
    }

    /// Returns the safe provider-route fingerprint used to share cooldown state.
    #[must_use]
    pub fn route_fingerprint(&self) -> &str {
        &self.route_fingerprint
    }
}

/// Wraps a provider-owned status error with provider-neutral rate-limit metadata for HTTP 429.
#[must_use]
pub fn provider_status_error(
    status: u16,
    retry_after_header: Option<&str>,
    error: anyhow::Error,
) -> anyhow::Error {
    if status == 429 {
        ProviderRateLimitError::new(error, retry_after_header).into()
    } else {
        error
    }
}

/// Finds provider-neutral rate-limit metadata through an anyhow context chain.
#[must_use]
pub fn provider_rate_limit_from_error(error: &anyhow::Error) -> Option<&ProviderRateLimitError> {
    error.downcast_ref::<ProviderRateLimitError>()
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

fn parse_retry_after_ms(value: &str, now: SystemTime) -> Option<u64> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(seconds.saturating_mul(1_000));
    }
    let not_before = httpdate::parse_http_date(value).ok()?;
    let duration = not_before.duration_since(now).unwrap_or(Duration::ZERO);
    Some(duration_millis_u64(duration))
}

#[cfg(test)]
#[path = "tests/provider_error_tests.rs"]
mod tests;
