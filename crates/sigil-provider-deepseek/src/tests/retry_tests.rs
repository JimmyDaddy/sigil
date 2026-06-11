use crate::errors::DeepSeekProviderError;

use super::classify_status;

#[test]
fn classify_status_maps_known_error_classes() {
    assert!(matches!(
        classify_status(401, "auth"),
        DeepSeekProviderError::Authentication(401)
    ));
    assert!(matches!(
        classify_status(402, "billing"),
        DeepSeekProviderError::Billing(402)
    ));
    assert!(matches!(
        classify_status(429, "rate"),
        DeepSeekProviderError::RateLimited
    ));
    assert!(matches!(
        classify_status(503, "server"),
        DeepSeekProviderError::RetryableStatus(503)
    ));
    assert!(matches!(
        classify_status(400, "bad request body"),
        DeepSeekProviderError::InvalidRequest(ref body) if body == "bad request body"
    ));
}
