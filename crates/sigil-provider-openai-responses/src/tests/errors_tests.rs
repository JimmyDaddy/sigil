use crate::errors::{OpenAiResponsesProviderError, classify_status};

#[test]
fn status_classification_keeps_auth_rate_limit_and_retryability_distinct() {
    assert!(matches!(
        classify_status(401, "no"),
        OpenAiResponsesProviderError::Authentication(401)
    ));
    assert!(matches!(
        classify_status(429, "slow"),
        OpenAiResponsesProviderError::RateLimited
    ));
    assert!(matches!(
        classify_status(503, "retry"),
        OpenAiResponsesProviderError::RetryableStatus(503)
    ));
}

#[test]
fn status_classification_accepts_only_exact_structured_context_rejection() {
    assert!(matches!(
        classify_status(
            400,
            r#"{"error":{"code":"context_length_exceeded","message":"too long"}}"#
        ),
        OpenAiResponsesProviderError::ContextWindowExceeded
    ));
    assert!(matches!(
        classify_status(400, r#"{"error":{"message":"context_length_exceeded"}}"#),
        OpenAiResponsesProviderError::Status { .. }
    ));
    assert!(matches!(
        classify_status(
            500,
            r#"{"error":{"code":"context_length_exceeded","message":"too long"}}"#
        ),
        OpenAiResponsesProviderError::RetryableStatus(500)
    ));
}
