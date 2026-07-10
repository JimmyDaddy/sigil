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
