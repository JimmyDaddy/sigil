use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use anyhow::{Result, anyhow};
use tokio::sync::oneshot;

use super::{AgentCompletionHub, AgentCompletionHubError, AgentCompletionRegistration};

#[tokio::test]
async fn duplicate_registration_rejects_the_whole_batch_before_polling() {
    let polls = Arc::new(AtomicUsize::new(0));
    let registration = |sequence| {
        let polls = Arc::clone(&polls);
        AgentCompletionRegistration::new("same-attempt", sequence, (), async move {
            polls.fetch_add(1, Ordering::SeqCst);
            Ok::<_, anyhow::Error>(sequence)
        })
    };

    let result = AgentCompletionHub::from_batch(vec![registration(0), registration(1)]);

    let rejection = match result {
        Ok(_) => panic!("duplicate registration should reject the batch"),
        Err(rejection) => rejection,
    };
    assert!(matches!(
        rejection.error(),
        AgentCompletionHubError::DuplicateRegistration { .. }
    ));
    assert_eq!(polls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn terminal_envelopes_arrive_once_in_completion_order() -> Result<()> {
    let (first_tx, first_rx) = oneshot::channel();
    let (second_tx, second_rx) = oneshot::channel();
    let registrations = vec![
        AgentCompletionRegistration::new("first", 0, "first-context", async move {
            Ok(first_rx.await?)
        }),
        AgentCompletionRegistration::new("second", 1, "second-context", async move {
            Ok(second_rx.await?)
        }),
    ];
    let hub = AgentCompletionHub::from_batch(registrations)
        .map_err(|rejection| rejection.into_error())?;

    second_tx
        .send("second-result")
        .map_err(|_| anyhow!("second receiver closed"))?;
    let collector = tokio::spawn(hub.collect());
    tokio::task::yield_now().await;
    first_tx
        .send("first-result")
        .map_err(|_| anyhow!("first receiver closed"))?;
    let envelopes = collector.await?;

    assert_eq!(envelopes.len(), 2);
    assert_eq!(envelopes[0].key, "second");
    assert_eq!(envelopes[0].sequence, 1);
    assert_eq!(envelopes[0].completion_index, 0);
    assert_eq!(envelopes[0].context, "second-context");
    assert_eq!(
        envelopes[0].result.as_ref().expect("second result"),
        &"second-result"
    );
    assert_eq!(envelopes[1].key, "first");
    assert_eq!(envelopes[1].sequence, 0);
    assert_eq!(envelopes[1].completion_index, 1);
    assert_eq!(envelopes[1].context, "first-context");
    assert_eq!(
        envelopes[1].result.as_ref().expect("first result"),
        &"first-result"
    );
    Ok(())
}

#[tokio::test]
async fn participant_failure_is_a_single_terminal_envelope() -> Result<()> {
    let registrations = vec![
        AgentCompletionRegistration::new("ok", 0, (), async { Ok::<_, anyhow::Error>(7) }),
        AgentCompletionRegistration::new("failed", 1, (), async {
            Err::<u8, _>(anyhow!("provider failed"))
        }),
    ];
    let envelopes = AgentCompletionHub::from_batch(registrations)
        .map_err(|rejection| rejection.into_error())?
        .collect()
        .await;

    assert_eq!(envelopes.len(), 2);
    assert_eq!(
        envelopes
            .iter()
            .filter(|envelope| envelope.key == "ok" && envelope.result.is_ok())
            .count(),
        1
    );
    assert_eq!(
        envelopes
            .iter()
            .filter(|envelope| envelope.key == "failed" && envelope.result.is_err())
            .count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn terminal_callback_observes_each_arrival_before_collection_finishes() -> Result<()> {
    let (first_tx, first_rx) = oneshot::channel();
    let (second_tx, second_rx) = oneshot::channel();
    let registrations = vec![
        AgentCompletionRegistration::new("first", 0, (), async move { Ok(first_rx.await?) }),
        AgentCompletionRegistration::new("second", 1, (), async move { Ok(second_rx.await?) }),
    ];
    second_tx
        .send("second-result")
        .map_err(|_| anyhow!("second receiver closed"))?;
    let first_sender = tokio::spawn(async move {
        tokio::task::yield_now().await;
        first_tx
            .send("first-result")
            .map_err(|_| anyhow!("first receiver closed"))
    });
    let mut observed = Vec::new();

    let envelopes = AgentCompletionHub::from_batch(registrations)
        .map_err(|rejection| rejection.into_error())?
        .collect_with(|envelope| {
            observed.push((envelope.key, envelope.sequence, envelope.completion_index));
        })
        .await;
    first_sender.await??;

    assert_eq!(envelopes.len(), 2);
    assert_eq!(observed, vec![("second", 1, 0), ("first", 0, 1)]);
    Ok(())
}
