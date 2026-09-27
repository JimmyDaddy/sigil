use super::*;
use crate::{ControlEntry, JsonlSessionStore, ModelMessage, Session, SessionLogEntry};
use anyhow::Result;

fn imported(target: &str) -> Result<BranchKnowledgeImportedV1> {
    let mut entry = BranchKnowledgeImportedV1 {
        schema_version: 1,
        import_id: String::new(),
        target_session_id: target.to_owned(),
        source_session_id: "source-branch".to_owned(),
        source_turn_digest: stable_event_hash(b"finalized source turn"),
        source_message_id: "source-final-answer".to_owned(),
        source_text_sha256: stable_event_hash(b"A branch conclusion"),
        summary_sha256: stable_event_hash(b"A branch conclusion"),
        summary: "A branch conclusion".to_owned(),
        truncated: false,
    };
    entry.import_id = entry.expected_import_id()?;
    Ok(entry)
}

fn durable_session(path: &std::path::Path) -> Result<Session> {
    let mut session = Session::new("test", "test-model").with_store(JsonlSessionStore::new(path)?);
    session.append_control(ControlEntry::SessionIdentity {
        provider_name: "test".to_owned(),
        model_name: "test-model".to_owned(),
        resolved_model_route: None,
    })?;
    Ok(session)
}

#[test]
fn branch_knowledge_append_is_destination_bound_idempotent_and_survives_restart() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("target.jsonl");
    let mut session = durable_session(&path)?;
    let imported = imported(session.session_scope_id())?;
    let initial = std::fs::read(&path)?;
    for invalid_digest in ["a".repeat(64), format!("sha512:{}", "a".repeat(64))] {
        let mut invalid = imported.clone();
        invalid.source_turn_digest = invalid_digest;
        invalid.import_id = invalid.expected_import_id()?;
        assert!(session.append_branch_knowledge(invalid).is_err());
        assert_eq!(std::fs::read(&path)?, initial);
    }
    assert!(session.append_branch_knowledge(imported.clone())?);
    let bytes = std::fs::read(&path)?;
    assert!(!session.append_branch_knowledge(imported.clone())?);
    assert_eq!(std::fs::read(&path)?, bytes);
    drop(session);
    let mut restored = Session::load_from_store_for_control(JsonlSessionStore::new(&path)?)?;
    assert!(!restored.append_branch_knowledge(imported.clone())?);
    assert_eq!(std::fs::read(&path)?, bytes);
    let mut other = durable_session(&temp.path().join("other.jsonl"))?;
    assert!(other.append_branch_knowledge(imported.clone()).is_err());
    let different = self::imported(other.session_scope_id())?;
    assert_ne!(imported.import_id, different.import_id);
    assert!(other.append_branch_knowledge(different)?);
    Ok(())
}

#[test]
fn branch_knowledge_unrelated_target_append_succeeds_but_altered_content_is_rejected() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("target.jsonl");
    let mut session = durable_session(&path)?;
    let imported = imported(session.session_scope_id())?;
    session.append_user_message(ModelMessage::user("new user message"))?;
    assert!(session.append_branch_knowledge(imported.clone())?);
    let bytes = std::fs::read(&path)?;
    let mut changed = imported;
    changed.summary.push_str(" injected");
    assert!(session.append_branch_knowledge(changed).is_err());
    assert_eq!(std::fs::read(&path)?, bytes);
    Ok(())
}

#[test]
fn branch_knowledge_enters_actual_request_as_untrusted_without_source_authority() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut session = durable_session(&temp.path().join("target.jsonl"))?;
    let entry = imported(session.session_scope_id())?;
    session.append_branch_knowledge(entry)?;
    let knowledge = branch_knowledge_context(session.session_scope_id(), session.entries())?;
    assert_eq!(knowledge.items.len(), 1);
    assert_eq!(
        knowledge.items[0].trust_level,
        ContextTrustLevel::ExternalUntrusted
    );
    session.append_user_message(ModelMessage::user("Consider the branch conclusion"))?;
    let request = session.build_request_with_transient_messages_and_context(
        temp.path(),
        &crate::MemoryConfig::with_enabled(false),
        Vec::new(),
        None,
        None,
        None,
        &[],
        RuntimeContextCandidates::default(),
    )?;
    let body = serde_json::to_string(&request)?;
    assert!(body.contains("A branch conclusion"));
    assert!(body.contains("Unverified external knowledge"));
    assert!(!session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(
            ControlEntry::ToolApproval(_)
                | ControlEntry::VerificationRecorded(_)
                | ControlEntry::TaskRun(_)
        )
    )));
    Ok(())
}

#[test]
fn branch_knowledge_concurrent_exact_imports_share_existing_writer_and_append_once() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("target.jsonl");
    let first = durable_session(&path)?;
    let imported = imported(first.session_scope_id())?;
    let second = Session::load_from_store_for_control(JsonlSessionStore::new(&path)?)?;
    let barrier = std::sync::Barrier::new(2);
    let appended = std::thread::scope(|threads| -> Result<Vec<bool>> {
        let handles = [first, second]
            .into_iter()
            .map(|mut session| {
                let imported = imported.clone();
                let barrier = &barrier;
                threads.spawn(move || -> Result<bool> {
                    barrier.wait();
                    let appended = session.append_branch_knowledge(imported)?;
                    assert_eq!(
                        session
                            .entries()
                            .iter()
                            .filter(|entry| matches!(
                                entry,
                                SessionLogEntry::Control(ControlEntry::BranchKnowledgeImportedV1(
                                    _
                                ))
                            ))
                            .count(),
                        1
                    );
                    Ok(appended)
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("owned import thread"))
            .collect()
    })?;
    assert_eq!(appended.into_iter().filter(|appended| *appended).count(), 1);
    let restored = Session::load_from_store_for_control(JsonlSessionStore::new(&path)?)?;
    assert_eq!(
        restored
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::BranchKnowledgeImportedV1(_))
            ))
            .count(),
        1
    );
    Ok(())
}

fn operation(
    entry: &BranchKnowledgeImportedV1,
    key: &str,
) -> Result<crate::ApplicationOperationBindingV1> {
    crate::ApplicationOperationBindingV1::new(
        entry.target_session_id.clone(),
        crate::sha256_hex(key.as_bytes()),
        crate::sha256_hex(b"exact branch selection"),
        crate::ApplicationOperationTargetV1::ImportBranchKnowledge {
            source_session_id: entry.source_session_id.clone(),
            source_turn_digest: entry.source_turn_digest.clone(),
            source_message_id: entry.source_message_id.clone(),
            source_text_sha256: entry.source_text_sha256.clone(),
            summary_sha256: entry.summary_sha256.clone(),
        },
    )
}

#[test]
fn branch_knowledge_distinct_application_operations_require_their_own_causal_receipt() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("target.jsonl");
    let mut session = durable_session(&path)?;
    let entry = imported(session.session_scope_id())?;
    let owner = session.application_operation_owner()?;
    for (key, duplicate) in [
        ("first user action", false),
        ("second explicit user action", true),
    ] {
        let binding = operation(&entry, key)?;
        owner.prepare(&binding)?;
        assert!(
            crate::session::reconcile_application_operation(&owner.read_handle(), &binding)?
                .is_none(),
            "historical identical knowledge is not this operation's proof"
        );
        session.bind_application_operation(binding.clone())?;
        assert_eq!(session.append_branch_knowledge(entry.clone())?, !duplicate);
        let proof =
            crate::session::reconcile_application_operation(&owner.read_handle(), &binding)?
                .expect("actual bound import commit");
        proof.validate_binding(&binding)?;
        let before = std::fs::read(&path)?;
        session.bind_application_operation(binding)?;
        assert!(
            session.append_branch_knowledge(entry.clone()).is_err(),
            "same K/F must reconcile instead of executing twice"
        );
        session.clear_application_operation();
        assert_eq!(std::fs::read(&path)?, before);
    }
    assert_eq!(
        branch_knowledge_context(session.session_scope_id(), session.entries())?
            .items
            .len(),
        1,
        "reaffirmed imports remain one semantic context item"
    );
    Ok(())
}

#[test]
fn branch_knowledge_causal_receipt_and_import_recover_as_one_writer_batch() -> Result<()> {
    for fault in [
        crate::session::SessionWriterFault::PartialFirstRecord,
        crate::session::SessionWriterFault::PartialSecondRecord,
        crate::session::SessionWriterFault::BeforeSync,
    ] {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("target.jsonl");
        let mut session = durable_session(&path)?;
        let entry = imported(session.session_scope_id())?;
        let binding = operation(&entry, "interrupted action")?;
        session.application_operation_owner()?.prepare(&binding)?;
        session.bind_application_operation(binding.clone())?;
        let store = JsonlSessionStore::new(&path)?;
        store.inject_writer_fault(fault)?;
        let _unconfirmed = session.append_branch_knowledge(entry);
        drop(session);
        let restored = Session::load_from_store_for_control(store)?;
        let owner = restored.application_operation_owner()?;
        crate::session::reconcile_application_operation(&owner.read_handle(), &binding)?
            .expect("recovered domain and causal marker")
            .validate_binding(&binding)?;
        assert_eq!(
            branch_knowledge_context(restored.session_scope_id(), restored.entries())?
                .items
                .len(),
            1
        );
    }
    Ok(())
}
