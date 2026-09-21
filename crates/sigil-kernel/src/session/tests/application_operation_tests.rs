use crate::session::{SessionWriterFault, reconcile_application_operation};
use crate::{
    ApplicationOperationBindingV1, ApplicationOperationTargetV1, ControlEntry,
    ConversationInputQueueControlAction, ConversationInputQueueControlEntry, JsonlSessionStore,
    ModelMessage, Session,
};
use anyhow::Result;

fn fixture() -> Result<(
    tempfile::TempDir,
    Session,
    JsonlSessionStore,
    ApplicationOperationBindingV1,
)> {
    let dir = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(dir.path().join("session.jsonl"))?;
    let mut session = Session::new("test", "model").with_store(store.clone());
    session.ensure_identity_entry()?;
    session.append_user_message(ModelMessage::user("owned session"))?;
    let binding = ApplicationOperationBindingV1::new(
        session.session_scope_id().to_owned(),
        "a".repeat(64),
        "b".repeat(64),
        ApplicationOperationTargetV1::QueuePause { paused: true },
    )?;
    Ok((dir, session, store, binding))
}

fn pause() -> ControlEntry {
    ControlEntry::ConversationInputQueueControl(ConversationInputQueueControlEntry {
        action: ConversationInputQueueControlAction::Pause,
        reason: Some("user control".to_owned()),
        updated_at_ms: Some(1),
    })
}

#[test]
fn operation_requires_original_prepare_and_reopens_actual_domain_batch() -> Result<()> {
    let (_dir, mut session, store, binding) = fixture()?;
    assert!(session.bind_application_operation(binding.clone()).is_err());
    // Cross a read page before the operation; current queue values are not its evidence.
    session.append_controls(vec![pause(); 140])?;
    let owner = session.application_operation_owner()?;
    owner.prepare(&binding)?;
    assert!(reconcile_application_operation(&owner.read_handle(), &binding)?.is_none());
    session.bind_application_operation(binding.clone())?;
    session.append_controls(vec![pause()])?;
    drop(session);
    let reopened = Session::load_from_store_for_control(store)?;
    let owner = reopened.application_operation_owner()?;
    let proof = reconcile_application_operation(&owner.read_handle(), &binding)?
        .expect("actual causal commit");
    assert!(proof.stream_sequence() > 0);
    proof.validate_binding(&binding)?;
    let records = owner.read_handle().read_event_records()?;
    let from_records = crate::session::reconcile_application_operation_records(&records, &binding)?
        .expect("same records reducer");
    assert_eq!(proof.event_id(), from_records.event_id());
    let mut other_target = binding.clone();
    other_target.target = ApplicationOperationTargetV1::QueuePause { paused: false };
    assert!(proof.validate_binding(&other_target).is_err());

    let mut conflicting = binding.clone();
    conflicting.fingerprint = "c".repeat(64);
    let conflicting = ApplicationOperationBindingV1::new(
        conflicting.session_scope_id,
        conflicting.reservation_key_digest,
        conflicting.fingerprint,
        conflicting.target,
    )?;
    assert!(owner.prepare(&conflicting).is_err());
    assert!(reconcile_application_operation(&owner.read_handle(), &conflicting).is_err());
    Ok(())
}

#[test]
fn operation_partial_domain_or_marker_write_recovers_one_crash_safe_batch() -> Result<()> {
    for fault in [
        SessionWriterFault::PartialFirstRecord,
        SessionWriterFault::PartialSecondRecord,
        SessionWriterFault::BeforeSync,
    ] {
        let (_dir, mut session, store, binding) = fixture()?;
        session.application_operation_owner()?.prepare(&binding)?;
        session.bind_application_operation(binding.clone())?;
        store.inject_writer_fault(fault)?;
        let _unconfirmed = session.append_controls(vec![pause()]);
        drop(session);
        let mut reopened = Session::load_from_store_for_control(store)?;
        let owner = reopened.application_operation_owner()?;
        assert!(
            reconcile_application_operation(&owner.read_handle(), &binding)?.is_some(),
            "{fault:?}"
        );
        reopened.bind_application_operation(binding.clone())?;
        assert!(
            reopened.append_controls(vec![pause()]).is_err(),
            "must reconcile instead of re-execute"
        );
        let records = owner.read_handle().read_event_records()?;
        assert_eq!(
            records
                .iter()
                .filter(|record| matches!(
                    record.session_log_entry(),
                    Ok(Some(crate::SessionLogEntry::Control(
                        ControlEntry::ConversationInputQueueControl(_)
                    )))
                ))
                .count(),
            1
        );
    }
    Ok(())
}

#[test]
fn unrelated_transition_cannot_settle_an_operation() -> Result<()> {
    let (_dir, mut session, _store, binding) = fixture()?;
    let owner = session.application_operation_owner()?;
    owner.prepare(&binding)?;
    session.bind_application_operation(binding.clone())?;
    session.append_control(ControlEntry::ConversationInputQueueControl(
        ConversationInputQueueControlEntry {
            action: ConversationInputQueueControlAction::Resume,
            reason: None,
            updated_at_ms: None,
        },
    ))?;
    assert!(reconcile_application_operation(&owner.read_handle(), &binding)?.is_none());
    session.append_control(pause())?;
    assert!(reconcile_application_operation(&owner.read_handle(), &binding)?.is_some());
    Ok(())
}

#[test]
fn bound_queue_keeps_its_cas_and_reopens_partial_batch_once() -> Result<()> {
    use crate::{
        ConversationQueueMutation, ConversationQueueMutationCommand, ConversationQueueRevision,
    };
    let (_dir, session, store, binding) = fixture()?;
    let owner = session.application_operation_owner()?;
    owner.prepare(&binding)?;
    store.inject_writer_fault(SessionWriterFault::PartialSecondRecord)?;
    let command = ConversationQueueMutationCommand {
        expected_queue_revision: ConversationQueueRevision::initial(),
        mutation: ConversationQueueMutation::Pause {
            reason: None,
            updated_at_ms: Some(1),
        },
    };
    let unconfirmed = owner.append_queue_mutation(&binding, command.clone());
    if let Err(error) = &unconfirmed {
        eprintln!("bound queue append: {error:#}");
    }
    drop(session);
    let reopened = Session::load_from_store_for_control(store.clone())?;
    let owner = reopened.application_operation_owner()?;
    assert!(reconcile_application_operation(&owner.read_handle(), &binding)?.is_some());
    let records = owner.read_handle().read_event_records()?;
    let domain = records
        .iter()
        .find(|record| {
            matches!(
                record.session_log_entry(),
                Ok(Some(crate::SessionLogEntry::Control(
                    ControlEntry::ConversationInputQueueControl(_)
                )))
            )
        })
        .expect("queue domain event")
        .stored_event();
    let marker = records
        .iter()
        .find(|record| {
            matches!(
                record.session_log_entry(),
                Ok(Some(crate::SessionLogEntry::Control(
                    ControlEntry::ApplicationOperationCommittedV1(_)
                )))
            )
        })
        .expect("queue operation marker")
        .stored_event();
    assert_eq!(marker.correlation_id, domain.correlation_id);
    assert_eq!(
        marker.causation_id.as_deref(),
        Some(domain.event_id.as_str())
    );
    let resume = ApplicationOperationBindingV1::new(
        binding.session_scope_id,
        "c".repeat(64),
        "d".repeat(64),
        ApplicationOperationTargetV1::QueuePause { paused: false },
    )?;
    owner.prepare(&resume)?;
    let before = std::fs::read(store.path())?;
    let stale = ConversationQueueMutationCommand {
        mutation: ConversationQueueMutation::Resume {
            reason: None,
            updated_at_ms: Some(2),
        },
        ..command
    };
    assert!(owner.append_queue_mutation(&resume, stale).is_err());
    assert_eq!(
        std::fs::read(store.path())?,
        before,
        "CAS failure must append neither domain fact nor marker"
    );
    assert!(reconcile_application_operation(&owner.read_handle(), &resume)?.is_none());
    Ok(())
}

#[test]
fn application_owner_observation_keeps_identity_and_never_repairs_a_tail() -> Result<()> {
    use std::io::Write;
    let (_dir, session, store, _binding) = fixture()?;
    let owner = session.application_operation_owner()?;
    let observed = owner.attach_for_observation()?;
    assert_eq!(observed.session_scope_id(), session.session_scope_id());
    assert_eq!(
        serde_json::to_value(observed.entries())?,
        serde_json::to_value(session.entries())?
    );
    assert_eq!(
        observed
            .application_operation_owner()?
            .domain_session_scope_id(),
        session.session_scope_id()
    );
    std::fs::OpenOptions::new()
        .append(true)
        .open(store.path())?
        .write_all(b"{\"interrupted_record\":")?;
    let before = std::fs::read(store.path())?;
    assert!(owner.attach_for_observation().is_err());
    assert_eq!(
        std::fs::read(store.path())?,
        before,
        "an operation query must not take recovery ownership"
    );
    Ok(())
}
