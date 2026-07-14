use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum OpenAiResponsesProviderError {
    #[error("missing OpenAI Responses API key")]
    MissingApiKey,
    #[error("OpenAI Responses provider does not support background requests")]
    BackgroundRequestsUnsupported,
    #[error("OpenAI Responses provider does not support provider-hosted tools")]
    HostedToolsUnsupported,
    #[error("OpenAI Responses provider does not support remote response handles")]
    ResponseHandlesUnsupported,
    #[error("OpenAI Responses provider supports reasoning effort low, medium, or high only")]
    UnsupportedReasoningEffort,
    #[error("OpenAI Responses authentication failed with status {0}")]
    Authentication(u16),
    #[error("OpenAI Responses request was rate limited")]
    RateLimited,
    #[error("OpenAI Responses backend returned retryable status {0}")]
    RetryableStatus(u16),
    #[error("OpenAI Responses rejected the request because its context window was exceeded")]
    ContextWindowExceeded,
    #[error("OpenAI Responses request failed with status {status}: {body}")]
    Status { status: u16, body: String },
}

pub fn classify_status(status: u16, body: &str) -> OpenAiResponsesProviderError {
    if status == 400 && openai_context_window_rejection(body) {
        return OpenAiResponsesProviderError::ContextWindowExceeded;
    }
    match status {
        401 | 403 => OpenAiResponsesProviderError::Authentication(status),
        429 => OpenAiResponsesProviderError::RateLimited,
        500..=599 => OpenAiResponsesProviderError::RetryableStatus(status),
        _ => OpenAiResponsesProviderError::Status {
            status,
            body: truncate_body(body),
        },
    }
}

fn openai_context_window_rejection(body: &str) -> bool {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|payload| {
            payload
                .pointer("/error/code")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .as_deref()
        == Some("context_length_exceeded")
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
