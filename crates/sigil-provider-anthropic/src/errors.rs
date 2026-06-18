use thiserror::Error;

#[derive(Debug, Error)]
pub enum AnthropicProviderError {
    #[error("missing Anthropic API key")]
    MissingApiKey,
    #[error("Anthropic authentication failed with status {0}")]
    Authentication(u16),
    #[error("Anthropic request was rate limited")]
    RateLimited,
    #[error("Anthropic backend returned retryable status {0}")]
    RetryableStatus(u16),
    #[error("Anthropic request failed with status {status}: {body}")]
    Status { status: u16, body: String },
    #[error("Anthropic stream error: {0}")]
    Stream(String),
}

pub fn classify_status(status: u16, body: &str) -> AnthropicProviderError {
    match status {
        401 | 403 => AnthropicProviderError::Authentication(status),
        429 => AnthropicProviderError::RateLimited,
        500..=599 => AnthropicProviderError::RetryableStatus(status),
        _ => AnthropicProviderError::Status {
            status,
            body: truncate_body(body),
        },
    }
}

fn truncate_body(body: &str) -> String {
    const MAX_CHARS: usize = 240;
    let mut chars = body.chars();
    let truncated = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

#[cfg(test)]
#[path = "tests/errors_tests.rs"]
mod tests;
