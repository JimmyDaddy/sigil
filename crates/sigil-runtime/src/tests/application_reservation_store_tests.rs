use super::*;
use sigil_application::{ApplicationCommandId, CommandRejection};

fn key() -> CommandReservationKey {
    CommandReservationKey {
        application_instance: sigil_application::ApplicationInstanceId::new("instance")
            .expect("instance"),
        principal: sigil_application::AuthenticatedSubject::new("subject").expect("subject"),
        client_epoch: 1,
        command_id: ApplicationCommandId::new("command").expect("command"),
    }
}

fn terminal() -> ApplicationCommandReceipt {
    ApplicationCommandReceipt::Rejected(CommandRejection {
        kind: "test".to_owned(),
        reason: "test rejection".to_owned(),
    })
}

#[test]
fn journal_replay_preserves_dispatch_and_terminal_state() {
    let key = key();
    let mut bytes = Vec::new();
    append_journal_bytes(
        &mut bytes,
        DurableReservationOperation::Reserve {
            key: key.clone(),
            fingerprint: "fingerprint".to_owned(),
        },
    )
    .expect("reserve");
    append_journal_bytes(
        &mut bytes,
        DurableReservationOperation::Reserve {
            key: key.clone(),
            fingerprint: "fingerprint".to_owned(),
        },
    )
    .expect("idempotent duplicate reserve");
    append_journal_bytes(
        &mut bytes,
        DurableReservationOperation::DispatchStarted {
            key: key.clone(),
            fingerprint: "fingerprint".to_owned(),
        },
    )
    .expect("dispatch");
    let receipt = terminal();
    append_journal_bytes(
        &mut bytes,
        DurableReservationOperation::Terminal {
            key: key.clone(),
            fingerprint: "fingerprint".to_owned(),
            receipt: Box::new(receipt.clone()),
        },
    )
    .expect("terminal");
    append_journal_bytes(
        &mut bytes,
        DurableReservationOperation::Terminal {
            key: key.clone(),
            fingerprint: "fingerprint".to_owned(),
            receipt: Box::new(receipt.clone()),
        },
    )
    .expect("idempotent duplicate terminal");

    let (entries, legacy) = decode_entries(&bytes).expect("replay");
    assert!(!legacy);
    let record = entries.get(&key).expect("record");
    assert_eq!(record.fingerprint, "fingerprint");
    assert!(matches!(record.state, DurableReservationState::Terminal(_)));
}

#[test]
fn journal_replay_rejects_terminal_rewrite_and_unknown_schema() {
    let key = key();
    let mut entries = BTreeMap::new();
    apply_journal_operation(
        &mut entries,
        DurableReservationOperation::Reserve {
            key: key.clone(),
            fingerprint: "fingerprint".to_owned(),
        },
    )
    .expect("reserve");
    apply_journal_operation(
        &mut entries,
        DurableReservationOperation::Terminal {
            key: key.clone(),
            fingerprint: "fingerprint".to_owned(),
            receipt: Box::new(terminal()),
        },
    )
    .expect("terminal");
    let error = apply_journal_operation(
        &mut entries,
        DurableReservationOperation::Terminal {
            key,
            fingerprint: "fingerprint".to_owned(),
            receipt: Box::new(ApplicationCommandReceipt::Rejected(CommandRejection {
                kind: "other".to_owned(),
                reason: "other rejection".to_owned(),
            })),
        },
    )
    .expect_err("terminal rewrite");
    assert!(matches!(error, ApplicationError::CorruptProjection(_)));

    let unknown = serde_json::json!({
        "schema_version": APPLICATION_RESERVATION_SCHEMA_VERSION + 1,
        "operation": {
            "type": "reserve",
            "value": { "key": {}, "fingerprint": "fingerprint" }
        }
    });
    let error = decode_entries(serde_json::to_string(&unknown).expect("json").as_bytes())
        .expect_err("unknown schema");
    assert!(matches!(error, ApplicationError::CorruptProjection(_)));
}

#[test]
fn legacy_snapshot_is_marked_for_one_time_migration() {
    let legacy = DurableReservationFile {
        schema_version: LEGACY_APPLICATION_RESERVATION_SCHEMA_VERSION,
        entries: vec![DurableReservationEntry {
            key: key(),
            fingerprint: "fingerprint".to_owned(),
            state: DurableReservationState::Reserved,
        }],
    };
    let bytes = serde_json::to_vec(&legacy).expect("legacy json");
    let (entries, legacy_snapshot) = decode_entries(&bytes).expect("decode legacy");
    assert!(legacy_snapshot);
    assert_eq!(entries.len(), 1);
}
