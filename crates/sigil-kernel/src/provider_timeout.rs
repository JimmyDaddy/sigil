use std::{future::Future, time::Duration};

use futures::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::time::{Instant, sleep_until, timeout};

use crate::ModelRequestTimeouts;

/// Provider-neutral phase where a model request timed out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderTimeoutPhase {
    RequestStart,
    StreamIdle,
    StreamTotal,
}

impl ProviderTimeoutPhase {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RequestStart => "request_start",
            Self::StreamIdle => "stream_idle",
            Self::StreamTotal => "stream_total",
        }
    }
}

impl std::fmt::Display for ProviderTimeoutPhase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Structured metadata for provider timeout diagnostics and UI hints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ProviderTimeoutMetadata {
    pub phase: ProviderTimeoutPhase,
    pub timeout_ms: u64,
    pub provider: String,
    pub model: String,
}

impl ProviderTimeoutMetadata {
    #[must_use]
    pub fn new(
        phase: ProviderTimeoutPhase,
        timeout: Duration,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            phase,
            timeout_ms: duration_millis_u64(timeout),
            provider: provider.into(),
            model: model.into(),
        }
    }
}

/// Mutable timeout state for one streamed provider response.
#[derive(Debug, Clone)]
pub struct ProviderStreamTimeoutState {
    total_deadline: Option<Instant>,
}

impl ProviderStreamTimeoutState {
    #[must_use]
    pub fn new(timeouts: ModelRequestTimeouts) -> Self {
        Self {
            total_deadline: timeouts
                .stream_total_timeout
                .map(|timeout| Instant::now() + timeout),
        }
    }
}

/// Applies the request-start timeout to a provider HTTP request future.
///
/// # Errors
///
/// Returns `ProviderTimeoutPhase::RequestStart` when the future does not complete in time.
pub async fn timeout_provider_request<F, T>(
    future: F,
    timeouts: ModelRequestTimeouts,
) -> Result<T, ProviderTimeoutPhase>
where
    F: Future<Output = T>,
{
    timeout(timeouts.request_timeout, future)
        .await
        .map_err(|_| ProviderTimeoutPhase::RequestStart)
}

/// Reads the next stream item with idle and optional total timeout enforcement.
///
/// # Errors
///
/// Returns `StreamIdle` when no chunk arrives before the idle timeout, or `StreamTotal` when the
/// optional total stream deadline has elapsed.
pub async fn timeout_provider_stream_next<S, T>(
    stream: &mut S,
    timeouts: ModelRequestTimeouts,
    state: &mut ProviderStreamTimeoutState,
) -> Result<Option<T>, ProviderTimeoutPhase>
where
    S: Stream<Item = T> + Unpin,
{
    if let Some(deadline) = state.total_deadline {
        if Instant::now() >= deadline {
            return Err(ProviderTimeoutPhase::StreamTotal);
        }

        let total_sleep = sleep_until(deadline);
        tokio::pin!(total_sleep);
        let idle_sleep = tokio::time::sleep(timeouts.stream_idle_timeout);
        tokio::pin!(idle_sleep);

        tokio::select! {
            item = stream.next() => Ok(item),
            () = &mut total_sleep => Err(ProviderTimeoutPhase::StreamTotal),
            () = &mut idle_sleep => Err(ProviderTimeoutPhase::StreamIdle),
        }
    } else {
        timeout(timeouts.stream_idle_timeout, stream.next())
            .await
            .map_err(|_| ProviderTimeoutPhase::StreamIdle)
    }
}

fn duration_millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "tests/provider_timeout_tests.rs"]
mod tests;
