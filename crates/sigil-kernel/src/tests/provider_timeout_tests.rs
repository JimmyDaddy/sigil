use std::time::Duration;

use futures::stream;

use crate::{
    ModelRequestTimeouts, ProviderStreamTimeoutState, ProviderTimeoutMetadata,
    ProviderTimeoutPhase, timeout_provider_request, timeout_provider_stream_next,
};

fn timeouts() -> ModelRequestTimeouts {
    ModelRequestTimeouts {
        request_timeout: Duration::from_millis(5),
        stream_idle_timeout: Duration::from_millis(5),
        stream_total_timeout: None,
    }
}

#[tokio::test]
async fn request_timeout_reports_request_start_phase() {
    let result = timeout_provider_request(
        async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            "done"
        },
        timeouts(),
    )
    .await;

    assert_eq!(result, Err(ProviderTimeoutPhase::RequestStart));
}

#[tokio::test]
async fn stream_next_reports_idle_phase() {
    let mut stream = stream::pending::<usize>();
    let mut state = ProviderStreamTimeoutState::new(timeouts());

    let result = timeout_provider_stream_next(&mut stream, timeouts(), &mut state).await;

    assert_eq!(result, Err(ProviderTimeoutPhase::StreamIdle));
}

#[tokio::test]
async fn stream_next_reports_total_phase_before_idle_when_total_deadline_wins() {
    let timeouts = ModelRequestTimeouts {
        request_timeout: Duration::from_millis(5),
        stream_idle_timeout: Duration::from_millis(50),
        stream_total_timeout: Some(Duration::from_millis(5)),
    };
    let mut stream = stream::pending::<usize>();
    let mut state = ProviderStreamTimeoutState::new(timeouts);

    let result = timeout_provider_stream_next(&mut stream, timeouts, &mut state).await;

    assert_eq!(result, Err(ProviderTimeoutPhase::StreamTotal));
}

#[tokio::test]
async fn stream_next_allows_active_stream_to_exceed_request_timeout() {
    let timeouts = ModelRequestTimeouts {
        request_timeout: Duration::from_millis(1),
        stream_idle_timeout: Duration::from_millis(50),
        stream_total_timeout: None,
    };
    let mut stream = stream::iter([1usize, 2]);
    let mut state = ProviderStreamTimeoutState::new(timeouts);

    assert_eq!(
        timeout_provider_stream_next(&mut stream, timeouts, &mut state).await,
        Ok(Some(1))
    );
    assert_eq!(
        timeout_provider_stream_next(&mut stream, timeouts, &mut state).await,
        Ok(Some(2))
    );
}

#[test]
fn timeout_metadata_uses_stable_phase_labels_and_millis() {
    let metadata = ProviderTimeoutMetadata::new(
        ProviderTimeoutPhase::StreamIdle,
        Duration::from_secs(2),
        "deepseek",
        "deepseek-v4-pro",
    );

    assert_eq!(ProviderTimeoutPhase::StreamIdle.as_str(), "stream_idle");
    assert_eq!(metadata.phase, ProviderTimeoutPhase::StreamIdle);
    assert_eq!(metadata.timeout_ms, 2_000);
    assert_eq!(metadata.provider, "deepseek");
    assert_eq!(metadata.model, "deepseek-v4-pro");
}
