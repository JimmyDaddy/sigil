use super::*;

fn event(sequence: u64) -> crate::PublicRunEvent {
    crate::PublicRunEvent::new(
        "session-1".to_owned(),
        "run-1".to_owned(),
        sequence,
        crate::PublicRunEventKind::Notice {
            message: "safe".to_owned(),
        },
    )
}

fn entry(sequence: u64) -> PublicEventOutboxEntryV1 {
    let event = event(sequence);
    PublicEventOutboxEntryV1 {
        schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
        public_event_id: format!("event-{sequence}"),
        domain_event_id: format!("domain-{sequence}"),
        run_id: event.run_id.clone(),
        sequence,
        payload_digest: crate::stable_event_hash(
            serde_json::to_vec(&event).expect("public event encodes"),
        ),
        event,
    }
}

#[test]
fn outbox_projection_keeps_failed_delivery_pending_without_changing_domain_event() -> Result<()> {
    let entry = entry(1);
    let mut projection = PublicEventOutboxProjectionV1::default();
    projection.apply_outbox(entry.clone())?;
    assert_eq!(projection.pending_for_adapter("http").len(), 1);
    projection.apply_delivery(PublicEventDeliveryReceiptV1 {
        schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
        public_event_id: entry.public_event_id.clone(),
        adapter: "http".to_owned(),
        delivered_at_unix_ms: 1,
    })?;
    assert!(projection.pending_for_adapter("http").is_empty());
    assert_eq!(projection.pending_for_adapter("desktop").len(), 1);
    Ok(())
}

#[test]
fn outbox_projection_orders_state_events_independently_of_delivery() -> Result<()> {
    let first = entry(1);
    let second = entry(2);
    let mut projection = PublicEventOutboxProjectionV1::default();
    projection.apply_outbox(first.clone())?;
    projection.apply_outbox(second)?;
    projection.apply_delivery(PublicEventDeliveryReceiptV1 {
        schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
        public_event_id: first.public_event_id,
        adapter: "tui".to_owned(),
        delivered_at_unix_ms: 1,
    })?;

    let events = projection.events_in_order();
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(events.len(), 2);
    assert!(projection.pending_for_adapter("tui").len() == 1);
    Ok(())
}
