use std::collections::BTreeSet;

use anyhow::Result;

use super::*;
use crate::{
    ControlEntry, DurableEventType, EventClass, JsonlSessionStore, ModelMessage, Session,
    SessionLogEntry, SessionStreamRecord, StoredEvent,
};

#[test]
fn composition_binding_is_critical_and_round_trips_without_configuration() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let mut session = Session::new("fixture", "model").with_store(JsonlSessionStore::new(&path)?);
    let snapshot = SessionCompositionSnapshotV1::new(BTreeSet::from([OptionalCapability::Skills]));
    session.append_control(ControlEntry::SessionCompositionBound(snapshot.clone()))?;
    let records = JsonlSessionStore::read_event_records(&path)?;
    let record = records
        .last()
        .expect("composition record persisted")
        .stored_event();
    assert_eq!(record.event_type, "session_composition_bound");
    assert_eq!(record.event_class, EventClass::Critical);
    let entries = JsonlSessionStore::read_entries(&path)?;
    let SessionLogEntry::Control(ControlEntry::SessionCompositionBound(actual)) =
        entries.last().expect("binding decoded")
    else {
        panic!("binding must retain its typed payload");
    };
    assert_eq!(actual, &snapshot);
    let payload = serde_json::to_value(actual)?;
    assert_eq!(payload.as_object().expect("object").len(), 3);
    Ok(())
}

#[test]
fn composition_rejects_unknown_execution_contracts() {
    let mut snapshot = SessionCompositionSnapshotV1::new(BTreeSet::new());
    snapshot.schema_version += 1;
    assert!(snapshot.validate().is_err());
    snapshot.schema_version = SESSION_COMPOSITION_SCHEMA_VERSION;
    snapshot.core_contract_version += 1;
    assert!(snapshot.validate().is_err());
}

fn assert_composition_payload_is_rejected(
    payload: serde_json::Value,
    expected_error: &str,
) -> Result<()> {
    let event = StoredEvent::new(
        DurableEventType::SessionCompositionBound,
        EventClass::Critical,
        "composition-event".to_owned(),
        "composition-session".to_owned(),
        1,
        payload,
    )?;
    event.verify_record_checksum()?;
    let line = event.to_json_line()?;
    let stored_error = JsonlSessionStore::session_entry_from_json_line(&line)
        .expect_err("stored composition event must reject an invalid binding payload");
    assert!(stored_error.to_string().contains(expected_error));

    let domain_error = SessionStreamRecord::Stored(event)
        .session_log_entry()
        .expect_err("domain composition event must reject an invalid binding payload");
    assert!(domain_error.to_string().contains(expected_error));

    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    std::fs::write(&path, line)?;
    let recovery_error = JsonlSessionStore::read_entries(&path)
        .expect_err("session recovery must not discard an invalid critical binding");
    assert!(recovery_error.to_string().contains(expected_error));
    Ok(())
}

#[test]
fn composition_event_rejects_missing_payload_with_valid_checksum() -> Result<()> {
    assert_composition_payload_is_rejected(
        serde_json::json!({}),
        "session composition event is missing its session_log_entry payload",
    )
}

#[test]
fn composition_event_rejects_different_entry_variants_with_valid_checksum() -> Result<()> {
    for entry in [
        SessionLogEntry::User(ModelMessage::user("queued input")),
        SessionLogEntry::Control(ControlEntry::Note {
            kind: "ordinary_control".to_owned(),
            data: serde_json::json!({}),
        }),
    ] {
        assert_composition_payload_is_rejected(
            serde_json::json!({ "session_log_entry": entry }),
            "session composition event carried a different session entry",
        )?;
    }
    Ok(())
}
