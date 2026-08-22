use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, anyhow};
use futures::{Stream, StreamExt};
use thiserror::Error;

use crate::{ProviderRequestRejection, SecretRedactor};

/// Typed provider timeout boundary. Adapters create this instead of encoding timeout phase in an
/// error string so recovery policy can remain provider-neutral and text-free.
#[derive(Debug, Clone, Error)]
#[error("provider request timed out during {phase}")]
pub struct ProviderTimeoutError {
    pub phase: crate::ProviderTimeoutPhase,
    pub timeout_ms: u64,
}

/// The provider transport ended without a terminal protocol frame. It is distinct from a parser
/// violation so V1 can recover only the zero-output case and block partial output safely.
#[derive(Debug, Clone, Error)]
#[error("provider stream ended before its terminal frame")]
pub struct ProviderStreamEndedUnexpectedly;

impl ProviderTimeoutError {
    #[must_use]
    pub fn new(phase: crate::ProviderTimeoutPhase, timeout: Duration) -> Self {
        Self {
            phase,
            timeout_ms: timeout.as_millis().try_into().unwrap_or(u64::MAX),
        }
    }
}

/// Provider-neutral point reached by a failed physical request.
///
/// Provider adapters own the mapping from their transport/protocol errors to this bounded
/// vocabulary. The kernel deliberately never inspects error text when it makes recovery
/// decisions.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderWireStateV1 {
    NoBytesSent,
    RequestBytesMayHaveBeenSent,
    ResponseStarted,
}

/// Provider-supplied scheduling information that is safe for recovery policy to consume.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderRetryHintV1 {
    None,
    RetryAfterMs(u64),
}

/// Provider-neutral classification of a failed physical attempt.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderFailureClassV1 {
    RejectedBeforeDispatch,
    RateLimited,
    TransientServer,
    TransportInterrupted,
    StreamEndedUnexpectedly,
    ProtocolViolation,
    ContextCapacity,
    Authentication,
    BillingOrQuota,
    RouteUnavailable,
    PermanentRequest,
    Cancelled,
}

/// Typed, safe failure observation emitted by a provider adapter.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderFailureObservationV1 {
    pub class: ProviderFailureClassV1,
    pub retry_after_ms: Option<u64>,
    pub wire_state: ProviderWireStateV1,
    pub provider_retry_hint: ProviderRetryHintV1,
    /// A bounded provider-neutral diagnostic code. Never include provider response text.
    pub safe_diagnostic_code: String,
}

impl ProviderFailureObservationV1 {
    /// Builds one adapter-owned bounded observation. The diagnostic code must be a stable,
    /// provider-neutral identifier, never an HTTP body or transport error string.
    #[must_use]
    pub fn classified(
        class: ProviderFailureClassV1,
        wire_state: ProviderWireStateV1,
        safe_diagnostic_code: impl Into<String>,
    ) -> Self {
        Self {
            class,
            retry_after_ms: None,
            provider_retry_hint: ProviderRetryHintV1::None,
            wire_state,
            safe_diagnostic_code: bounded_safe_diagnostic_code(safe_diagnostic_code.into()),
        }
    }

    /// Builds the conservative default used when an adapter did not provide a richer mapping.
    #[must_use]
    pub fn from_known_error(
        error: &anyhow::Error,
        rejection: Option<ProviderRequestRejection>,
        wire_state: ProviderWireStateV1,
    ) -> Self {
        if error
            .chain()
            .any(|cause| cause.downcast_ref::<ProviderTimeoutError>().is_some())
        {
            return Self::transport_interrupted(wire_state);
        }
        if error.chain().any(|cause| {
            cause
                .downcast_ref::<ProviderStreamEndedUnexpectedly>()
                .is_some()
        }) {
            return Self::classified(
                ProviderFailureClassV1::StreamEndedUnexpectedly,
                wire_state,
                "provider_stream_ended_unexpectedly",
            );
        }
        if let Some(rate_limit) = provider_rate_limit_from_error(error) {
            let retry_after_ms = rate_limit.retry_after_ms();
            return Self {
                class: ProviderFailureClassV1::RateLimited,
                retry_after_ms,
                wire_state,
                provider_retry_hint: retry_after_ms
                    .map(ProviderRetryHintV1::RetryAfterMs)
                    .unwrap_or(ProviderRetryHintV1::None),
                safe_diagnostic_code: "provider_rate_limited".to_owned(),
            };
        }
        let (class, code) = match rejection {
            Some(ProviderRequestRejection::ConnectFailedBeforeDispatch) => (
                ProviderFailureClassV1::RejectedBeforeDispatch,
                "connect_rejected_before_dispatch",
            ),
            Some(ProviderRequestRejection::RateLimited) => {
                (ProviderFailureClassV1::RateLimited, "provider_rate_limited")
            }
            Some(ProviderRequestRejection::ContextWindowExceeded) => {
                (ProviderFailureClassV1::ContextCapacity, "context_capacity")
            }
            None => (
                ProviderFailureClassV1::PermanentRequest,
                "provider_error_unclassified",
            ),
        };
        Self {
            class,
            retry_after_ms: None,
            wire_state,
            provider_retry_hint: ProviderRetryHintV1::None,
            safe_diagnostic_code: code.to_owned(),
        }
    }

    /// Builds a bounded transport observation without exposing a provider error chain.
    #[must_use]
    pub fn transport_interrupted(wire_state: ProviderWireStateV1) -> Self {
        Self::classified(
            ProviderFailureClassV1::TransportInterrupted,
            wire_state,
            "provider_transport_interrupted",
        )
    }
}

fn bounded_safe_diagnostic_code(value: String) -> String {
    let valid = !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    if valid {
        value
    } else {
        "provider_failure_classified".to_owned()
    }
}

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
