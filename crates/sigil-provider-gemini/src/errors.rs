use thiserror::Error;

#[derive(Debug, Error)]
pub enum GeminiProviderError {
    #[error("missing Gemini API key")]
    MissingApiKey,
    #[error("Gemini authentication failed with status {0}")]
    Authentication(u16),
    #[error("Gemini request was rate limited")]
    RateLimited,
    #[error("Gemini backend returned retryable status {0}")]
    RetryableStatus(u16),
    #[error("Gemini request failed with status {status}: {body}")]
    Status { status: u16, body: String },
    #[error("Gemini response was blocked: {0}")]
    Blocked(String),
    #[error("Gemini response finished abnormally: {reason}{message}")]
    AbnormalFinish { reason: String, message: String },
    #[error("Gemini returned grounding metadata for a request without hosted search")]
    UnexpectedGroundingMetadata,
}

pub fn classify_status(status: u16, body: &str) -> GeminiProviderError {
    match status {
        401 | 403 => GeminiProviderError::Authentication(status),
        429 => GeminiProviderError::RateLimited,
        500..=599 => GeminiProviderError::RetryableStatus(status),
        _ => GeminiProviderError::Status {
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
