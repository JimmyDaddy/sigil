use super::*;
use sigil_application::{
    ApplicationCommandId, ApplicationCommandReceipt, ApplicationDomainCommitRef,
    ApplicationDomainReceipt, ApplicationFrontier, ApplicationInstanceId, ApplicationScope,
    AuthenticatedSubject, CommandEffectBinding, CommandLifecyclePhase, CommandRecoveryBinding,
    EffectSettlementClass, WorkspaceScopeId,
};

fn key() -> CommandReservationKey {
    let principal = AuthenticatedSubject::new("subject").expect("subject");
    let scope = ApplicationScope {
        application_instance: ApplicationInstanceId::new("instance").expect("instance"),
        authenticated_subject: principal.clone(),
        workspace: Some(WorkspaceScopeId::new("workspace").expect("workspace")),
        session: None,
    };
    CommandReservationKey {
        application_instance: scope.application_instance.clone(),
        authority_scope: scope,
        principal,
        client_epoch: 1,
        command_id: ApplicationCommandId::new("command").expect("command"),
    }
}

fn fingerprint() -> String {
    "a".repeat(64)
}

fn effect_binding(key: &CommandReservationKey) -> CommandEffectBinding {
    CommandEffectBinding {
        command_id: key.command_id.clone(),
        command_kind: "test".to_owned(),
        reservation_fingerprint: fingerprint(),
        recovery: CommandRecoveryBinding {
            key: key.clone(),
            phase: CommandLifecyclePhase::EffectStarted,
        },
        owner_effect_id: "effect-1".to_owned(),
    }
}

fn domain_receipt(key: &CommandReservationKey) -> ApplicationDomainReceipt {
    ApplicationDomainReceipt {
        command_id: key.command_id.clone(),
        command_kind: "test".to_owned(),
        frontier: ApplicationFrontier {
            schema_version: sigil_application::APPLICATION_CONTRACT_SCHEMA_VERSION,
            scope: key.authority_scope.clone(),
            writer_generation: 1,
            stream_generation: 1,
            through_sequence: 1,
            durable_cursor: "fixture-domain-1".to_owned(),
        },
        settlement: EffectSettlementClass::AtomicDurableMutation,
        summary: "fixture domain commit".to_owned(),
        domain_commit: ApplicationDomainCommitRef {
            source_event_id: "fixture-domain-event-1".to_owned(),
            source_sequence: 1,
            source_digest: fingerprint(),
        },
        outcome: None,
    }
}

fn append_entry(bytes: &mut Vec<u8>, operation: DurableReservationOperation) {
    let entry = DurableReservationJournalEntry {
        schema_version: APPLICATION_RESERVATION_SCHEMA_VERSION,
        operation,
    };
    bytes.extend(serde_json::to_vec(&entry).expect("journal entry"));
    bytes.push(b'\n');
}

#[test]
fn journal_replay_preserves_each_monotonic_lifecycle_phase() {
    let key = key();
    let fingerprint = fingerprint();
    let binding = effect_binding(&key);
    let domain = domain_receipt(&key);
    let mut bytes = Vec::new();
    append_entry(
        &mut bytes,
        DurableReservationOperation::Reserve {
            key: key.clone(),
            fingerprint: fingerprint.clone(),
        },
    );
    append_entry(
        &mut bytes,
        DurableReservationOperation::DispatchStarted {
            key: key.clone(),
            fingerprint: fingerprint.clone(),
        },
    );
    append_entry(
        &mut bytes,
        DurableReservationOperation::EffectStarted {
            key: key.clone(),
            fingerprint: fingerprint.clone(),
            binding: Box::new(binding),
        },
    );
    append_entry(
        &mut bytes,
        DurableReservationOperation::DomainCommitted {
            key: key.clone(),
            fingerprint: fingerprint.clone(),
            receipt: Box::new(domain.clone()),
        },
    );
    append_entry(
        &mut bytes,
        DurableReservationOperation::Settled {
            key: key.clone(),
            fingerprint,
            receipt: Box::new(ApplicationCommandReceipt::Settled(domain)),
        },
    );

    let entries = decode_entries(&bytes).expect("replay");
    let record = entries.get(&key).expect("record");
    assert!(matches!(record.state, DurableReservationState::Settled(_)));
}

#[test]
fn replayed_domain_commit_is_a_terminal_admission_before_settlement_index_exists() {
    let key = key();
    let fingerprint = fingerprint();
    let domain = domain_receipt(&key);
    let mut entries = BTreeMap::new();
    apply_journal_operation(
        &mut entries,
        DurableReservationOperation::Reserve {
            key: key.clone(),
            fingerprint: fingerprint.clone(),
        },
    )
    .expect("reserve");
    apply_journal_operation(
        &mut entries,
        DurableReservationOperation::DispatchStarted {
            key: key.clone(),
            fingerprint: fingerprint.clone(),
        },
    )
    .expect("dispatch");
    apply_journal_operation(
        &mut entries,
        DurableReservationOperation::EffectStarted {
            key: key.clone(),
            fingerprint: fingerprint.clone(),
            binding: Box::new(effect_binding(&key)),
        },
    )
    .expect("effect");
    apply_journal_operation(
        &mut entries,
        DurableReservationOperation::DomainCommitted {
            key: key.clone(),
            fingerprint: fingerprint.clone(),
            receipt: Box::new(domain.clone()),
        },
    )
    .expect("domain commit");

    let admission = existing_admission(
        entries.get(&key).expect("record"),
        &key,
        key.command_id.clone(),
        "test".to_owned(),
        fingerprint,
    );
    assert!(matches!(
        admission,
        RuntimeApplicationReservationAdmission::Existing(receipt)
            if matches!(&*receipt, ApplicationCommandReceipt::Settled(receipt) if *receipt == domain)
    ));
}

#[test]
fn pre_effect_replay_requires_repair_instead_of_claiming_in_flight() {
    let key = key();
    let fingerprint = fingerprint();
    let mut entries = BTreeMap::new();
    apply_journal_operation(
        &mut entries,
        DurableReservationOperation::Reserve {
            key: key.clone(),
            fingerprint: fingerprint.clone(),
        },
    )
    .expect("reserve");

    for expected_phase in [
        CommandLifecyclePhase::Reserved,
        CommandLifecyclePhase::DispatchStarted,
    ] {
        if expected_phase == CommandLifecyclePhase::DispatchStarted {
            apply_journal_operation(
                &mut entries,
                DurableReservationOperation::DispatchStarted {
                    key: key.clone(),
                    fingerprint: fingerprint.clone(),
                },
            )
            .expect("dispatch marker");
        }
        let admission = existing_admission(
            entries.get(&key).expect("record"),
            &key,
            key.command_id.clone(),
            "test".to_owned(),
            fingerprint.clone(),
        );
        assert!(matches!(
            admission,
            RuntimeApplicationReservationAdmission::Existing(receipt)
                if matches!(&*receipt, ApplicationCommandReceipt::Uncertain(receipt)
                    if receipt.recovery.phase == expected_phase)
        ));
    }
}

#[test]
fn journal_replay_rejects_non_monotonic_transition_and_unknown_schema() {
    let key = key();
    let mut entries = BTreeMap::new();
    apply_journal_operation(
        &mut entries,
        DurableReservationOperation::Reserve {
            key: key.clone(),
            fingerprint: fingerprint(),
        },
    )
    .expect("reserve");
    let error = apply_journal_operation(
        &mut entries,
        DurableReservationOperation::DomainCommitted {
            key: key.clone(),
            fingerprint: fingerprint(),
            receipt: Box::new(domain_receipt(&key)),
        },
    )
    .expect_err("domain commit before effect");
    assert!(matches!(error, ApplicationError::CorruptProjection(_)));

    let unknown = serde_json::json!({
        "schema_version": APPLICATION_RESERVATION_SCHEMA_VERSION + 1,
        "operation": { "type": "reserve", "value": { "key": {}, "fingerprint": "bad" } }
    });
    let error = decode_entries(serde_json::to_string(&unknown).expect("json").as_bytes())
        .expect_err("unknown schema");
    assert!(matches!(error, ApplicationError::CorruptProjection(_)));
}

#[test]
fn legacy_snapshot_is_not_silently_migrated() {
    let legacy = serde_json::json!({
        "schema_version": 1,
        "entries": []
    });
    let error = decode_entries(
        serde_json::to_string(&legacy)
            .expect("legacy json")
            .as_bytes(),
    )
    .expect_err("legacy format must fail closed");
    assert!(matches!(error, ApplicationError::CorruptProjection(_)));
}
