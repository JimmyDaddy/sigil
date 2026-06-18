use super::*;

#[test]
fn classify_status_groups_common_provider_failures() {
    assert!(matches!(
        classify_status(401, "nope"),
        AnthropicProviderError::Authentication(401)
    ));
    assert!(matches!(
        classify_status(429, "slow down"),
        AnthropicProviderError::RateLimited
    ));
    assert!(matches!(
        classify_status(503, "retry"),
        AnthropicProviderError::RetryableStatus(503)
    ));
    assert!(matches!(
        classify_status(400, "bad request"),
        AnthropicProviderError::Status { status: 400, .. }
    ));
}

#[test]
fn classify_status_truncates_large_error_bodies() {
    let body = "x".repeat(260);
    let error = classify_status(400, &body).to_string();

    assert!(error.contains("..."));
    assert!(error.len() < 340);
}
