use std::{io, time::Duration};

use futures::stream;

use super::*;

#[tokio::test]
async fn error_body_reader_caps_bytes_and_redacts_known_and_shaped_secrets() -> anyhow::Result<()> {
    let secret = "sk-provider-secret";
    let body = format!("token=visible {secret} {}", "x".repeat(128));
    let result = read_provider_error_body(
        stream::iter([Ok::<_, io::Error>(body.into_bytes())]),
        Duration::from_secs(1),
        48,
        &SecretRedactor::from_values([secret]),
    )
    .await?;

    assert_eq!(result.captured_bytes(), 48);
    assert!(result.truncated());
    assert!(result.text().len() <= 48);
    assert!(!result.text().contains(secret));
    assert!(!result.text().contains("visible"));
    assert!(result.text().contains("[redacted]"));
    Ok(())
}

#[tokio::test]
async fn error_body_reader_redacts_secret_prefix_split_at_cap() -> anyhow::Result<()> {
    let secret = "sk-provider-secret";
    let body = format!("diagnostic {secret} continues");
    let cap = "diagnostic sk-provider-s".len();
    let result = read_provider_error_body(
        stream::iter([Ok::<_, io::Error>(body.into_bytes())]),
        Duration::from_secs(1),
        cap,
        &SecretRedactor::from_values([secret]),
    )
    .await?;

    assert!(result.truncated());
    assert_eq!(result.text(), "diagnostic [redacted]");
    Ok(())
}

#[tokio::test]
async fn error_body_reader_times_out_with_context() {
    let error = read_provider_error_body(
        stream::pending::<Result<Vec<u8>, io::Error>>(),
        Duration::from_millis(10),
        PROVIDER_ERROR_BODY_LIMIT_BYTES,
        &SecretRedactor::empty(),
    )
    .await
    .expect_err("pending body must time out");

    assert!(
        error
            .to_string()
            .contains("provider error response body timed out after 10 ms")
    );
}

#[tokio::test]
async fn error_body_reader_preserves_stream_failure_context() {
    let error = read_provider_error_body(
        stream::iter([Err::<Vec<u8>, _>(io::Error::other("socket reset"))]),
        Duration::from_secs(1),
        PROVIDER_ERROR_BODY_LIMIT_BYTES,
        &SecretRedactor::empty(),
    )
    .await
    .expect_err("failed body stream must surface an error");

    assert!(
        error
            .to_string()
            .contains("failed to read provider error response body chunk")
    );
    assert!(format!("{error:#}").contains("socket reset"));
}

#[test]
fn provider_rate_limit_envelope_parses_delta_seconds_and_preserves_source() {
    let error = provider_status_error(
        429,
        Some("7"),
        anyhow::anyhow!("provider-specific rate limit"),
    );
    let rate_limit = provider_rate_limit_from_error(&error).expect("typed rate limit");

    assert_eq!(rate_limit.retry_after_ms(), Some(7_000));
    assert_eq!(error.to_string(), "provider-specific rate limit");
}

#[test]
fn retry_after_parser_accepts_http_date_and_clamps_past_dates_to_zero() {
    let now = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    let future = httpdate::fmt_http_date(now + Duration::from_millis(2_500));
    let past = httpdate::fmt_http_date(now - Duration::from_secs(1));

    assert_eq!(parse_retry_after_ms(&future, now), Some(2_000));
    assert_eq!(parse_retry_after_ms(&past, now), Some(0));
    assert_eq!(parse_retry_after_ms("not-a-date", now), None);
}

#[test]
fn provider_status_error_does_not_wrap_non_rate_limit_status() {
    let error = provider_status_error(503, Some("5"), anyhow::anyhow!("retryable backend"));

    assert!(provider_rate_limit_from_error(&error).is_none());
    assert_eq!(error.to_string(), "retryable backend");
}

#[test]
fn provider_route_cooldown_error_exposes_only_bounded_scheduling_metadata() {
    let error = ProviderRouteCooldownError::new(1_250, "sha256:test-route");

    assert_eq!(error.retry_after_ms(), 1_250);
    assert_eq!(error.route_fingerprint(), "sha256:test-route");
    assert_eq!(
        error.to_string(),
        "provider route is cooling down; retry after 1250 ms (route sha256:test-route)"
    );
}
