use thiserror::Error;

#[derive(Debug, Error)]
pub enum OpenAiCompatibleProviderError {
    #[error("missing OpenAI-compatible API key")]
    MissingApiKey,
    #[error("OpenAI-compatible authentication failed with status {0}")]
    Authentication(u16),
    #[error("OpenAI-compatible request was rate limited")]
    RateLimited,
    #[error("OpenAI-compatible backend returned retryable status {0}")]
    RetryableStatus(u16),
    #[error("OpenAI-compatible request failed with status {status}: {body}")]
    Status { status: u16, body: String },
}

pub fn classify_status(status: u16, body: &str) -> OpenAiCompatibleProviderError {
    match status {
        401 | 403 => OpenAiCompatibleProviderError::Authentication(status),
        429 => OpenAiCompatibleProviderError::RateLimited,
        500..=599 => OpenAiCompatibleProviderError::RetryableStatus(status),
        _ => OpenAiCompatibleProviderError::Status {
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
