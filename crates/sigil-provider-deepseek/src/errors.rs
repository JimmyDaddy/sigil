use thiserror::Error;

#[derive(Debug, Error)]
pub enum DeepSeekProviderError {
    #[error("missing api key")]
    MissingApiKey,
    #[error("deepseek auth failed with status {0}")]
    Authentication(u16),
    #[error("deepseek billing failed with status {0}")]
    Billing(u16),
    #[error("deepseek rate limited")]
    RateLimited,
    #[error("deepseek retryable server error {0}")]
    RetryableStatus(u16),
    #[error("deepseek invalid request: {0}")]
    InvalidRequest(String),
}

/// Response-body failure that preserves the typed `reqwest` source for kernel retry
/// classification while retaining a bounded provider-specific diagnostic code.
#[derive(Debug, Error)]
#[error("deepseek messages stream read failed [{kind}]: {source}")]
pub(crate) struct DeepSeekMessagesStreamReadError {
    pub(crate) kind: &'static str,
    #[source]
    pub(crate) source: reqwest::Error,
}
