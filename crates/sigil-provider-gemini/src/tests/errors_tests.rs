use super::*;

#[test]
fn classify_status_groups_common_provider_failures() {
    assert!(matches!(
        classify_status(403, "nope"),
        GeminiProviderError::Authentication(403)
    ));
    assert!(matches!(
        classify_status(429, "slow down"),
        GeminiProviderError::RateLimited
    ));
    assert!(matches!(
        classify_status(502, "retry"),
        GeminiProviderError::RetryableStatus(502)
    ));
    assert!(matches!(
        classify_status(400, "bad request"),
        GeminiProviderError::Status { status: 400, .. }
    ));
}

#[test]
fn classify_status_truncates_large_error_bodies() {
    let body = "x".repeat(260);
    let error = classify_status(400, &body).to_string();

    assert!(error.contains("..."));
    assert!(error.len() < 340);
}

#[test]
fn rate_limit_status_preserves_provider_neutral_retry_after() {
    let error = sigil_kernel::provider_status_error(
        429,
        Some("3"),
        classify_status(429, "slow down").into(),
    );

    assert_eq!(
        sigil_kernel::provider_rate_limit_from_error(&error)
            .and_then(|rate_limit| rate_limit.retry_after_ms()),
        Some(3_000)
    );
    assert!(
        error
            .to_string()
            .contains("Gemini request was rate limited")
    );
}
