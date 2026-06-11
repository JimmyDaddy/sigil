use crate::errors::DeepSeekProviderError;

pub fn classify_status(status: u16, body: &str) -> DeepSeekProviderError {
    match status {
        401 | 403 => DeepSeekProviderError::Authentication(status),
        402 => DeepSeekProviderError::Billing(status),
        429 => DeepSeekProviderError::RateLimited,
        500..=599 => DeepSeekProviderError::RetryableStatus(status),
        _ => DeepSeekProviderError::InvalidRequest(body.to_owned()),
    }
}

#[cfg(test)]
#[path = "tests/retry_tests.rs"]
mod tests;
