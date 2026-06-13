use crate::errors::{OpenAiCompatibleProviderError, classify_status};

#[test]
fn classify_status_maps_common_provider_failures() {
    assert!(matches!(
        classify_status(401, ""),
        OpenAiCompatibleProviderError::Authentication(401)
    ));
    assert!(matches!(
        classify_status(403, ""),
        OpenAiCompatibleProviderError::Authentication(403)
    ));
    assert!(matches!(
        classify_status(429, ""),
        OpenAiCompatibleProviderError::RateLimited
    ));
    assert!(matches!(
        classify_status(503, ""),
        OpenAiCompatibleProviderError::RetryableStatus(503)
    ));
    assert!(matches!(
        classify_status(400, "bad request"),
        OpenAiCompatibleProviderError::Status {
            status: 400,
            ref body
        } if body == "bad request"
    ));
}

#[test]
fn classify_status_truncates_large_error_body() {
    let body = "x".repeat(300);
    let error = classify_status(418, &body);

    assert!(matches!(
        error,
        OpenAiCompatibleProviderError::Status {
            status: 418,
            ref body
        } if body.len() == 243 && body.ends_with("...")
    ));
}
