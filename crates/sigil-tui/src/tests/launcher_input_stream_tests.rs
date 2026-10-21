use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
    time::Duration,
};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use futures::{StreamExt, stream};

use super::{EVENT_BATCH_LIMIT, poll_ready_input};

#[tokio::test]
async fn ready_input_preserves_key_paste_resize_and_cancel_order() {
    let events = vec![
        Event::Key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE)),
        Event::Paste("first\nsecond".to_owned()),
        Event::Resize(100, 30),
        Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
    ];
    let mut input = stream::iter(events.clone());
    let mut received = Vec::new();
    while let Some(Some(event)) = poll_ready_input(Box::pin(input.next()).as_mut()).await {
        received.push(event);
    }
    assert_eq!(received, events);
}

#[tokio::test]
async fn ready_input_batch_does_not_consume_past_event_budget() {
    let mut input = stream::iter(0..EVENT_BATCH_LIMIT + 3);
    let mut batch = Vec::new();
    for _ in 0..EVENT_BATCH_LIMIT {
        batch.push(
            poll_ready_input(Box::pin(input.next()).as_mut())
                .await
                .expect("buffered stream must be ready")
                .expect("buffered event must exist"),
        );
    }
    assert_eq!(batch, (0..EVENT_BATCH_LIMIT).collect::<Vec<_>>());
    assert_eq!(input.next().await, Some(EVENT_BATCH_LIMIT));
}

struct FirstWakerInput {
    ready: Arc<AtomicBool>,
    polls: Arc<AtomicUsize>,
    first_waker: Arc<Mutex<Option<Waker>>>,
}

impl Future for FirstWakerInput {
    type Output = u8;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.polls.fetch_add(1, Ordering::Relaxed);
        if self.ready.load(Ordering::Acquire) {
            return Poll::Ready(42);
        }
        self.first_waker
            .lock()
            .expect("test waker lock")
            .get_or_insert_with(|| context.waker().clone());
        Poll::Pending
    }
}

#[tokio::test]
async fn pending_input_is_polled_once_and_retains_a_live_owner_waker() {
    let ready = Arc::new(AtomicBool::new(false));
    let polls = Arc::new(AtomicUsize::new(0));
    let first_waker = Arc::new(Mutex::new(None));
    let mut pending = Box::pin(FirstWakerInput {
        ready: Arc::clone(&ready),
        polls: Arc::clone(&polls),
        first_waker: Arc::clone(&first_waker),
    });

    assert_eq!(poll_ready_input(pending.as_mut()).await, None);
    assert_eq!(polls.load(Ordering::Relaxed), 1);
    let producer = tokio::spawn(async move {
        tokio::task::yield_now().await;
        ready.store(true, Ordering::Release);
        first_waker
            .lock()
            .expect("test waker lock")
            .as_ref()
            .expect("initial probe must register an owner waker")
            .wake_by_ref();
    });
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), pending.as_mut())
            .await
            .expect("pending input must wake the owner"),
        42,
    );
    producer.await.expect("test producer must join");
}

#[tokio::test]
async fn ready_input_keeps_stream_errors_and_end_distinct_from_pending() {
    let mut input = stream::iter([Err::<Event, _>(std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "test input failure",
    ))]);
    let event = poll_ready_input(Box::pin(input.next()).as_mut())
        .await
        .expect("error event must be ready")
        .expect("error event is not end of stream");
    assert_eq!(
        event.expect_err("error must survive").kind(),
        std::io::ErrorKind::BrokenPipe
    );
    assert!(matches!(
        poll_ready_input(Box::pin(input.next()).as_mut()).await,
        Some(None)
    ));
}
