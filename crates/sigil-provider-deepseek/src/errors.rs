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
