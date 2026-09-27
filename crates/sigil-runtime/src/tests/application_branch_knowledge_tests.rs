use super::*;
use sigil_kernel::{
    ControlEntry, ConversationTurnForkRequest, EventClass, JsonlSessionStore, ModelMessage,
    fork_conversation_at_turn,
};

fn session(path: &std::path::Path) -> Result<Session> {
    let mut session = Session::new("test", "test-model").with_store(JsonlSessionStore::new(path)?);
    session.append_control(ControlEntry::SessionIdentity {
        provider_name: "test".to_owned(),
        model_name: "test-model".to_owned(),
        resolved_model_route: None,
    })?;
    Ok(session)
}

fn final_turn(session: &mut Session, text: &str) -> Result<()> {
    session.append_user_message(ModelMessage::user("Explore a possible solution"))?;
    let assistant = ModelMessage::assistant(Some(text.to_owned()), Vec::new());
    session.append_assistant_message(assistant.clone())?;
    session.append_durable_event(
        DurableEventType::RunFinalized,
        EventClass::Critical,
        serde_json::json!({"run_status":"completed","terminal_reason":"final_answer",
            "final_message_id":assistant.id,"tool_calls":0,"error":null}),
    )?;
    Ok(())
}

fn request(
    preview: &ApplicationBranchKnowledgePreview,
    index: usize,
) -> ApplicationBranchKnowledgeImportRequest {
    let point = &preview.points[index];
    ApplicationBranchKnowledgeImportRequest {
        source_session_ref: preview.source_session_ref.clone(),
        source_session_id: preview.source_session_id.clone(),
        source_turn_digest: point.source_turn_digest.clone(),
        source_message_id: point.source_message_id.clone(),
        source_text_sha256: point.source_text_sha256.clone(),
        summary_sha256: point.summary_sha256.clone(),
    }
}

#[test]
fn application_branch_knowledge_real_fork_lineage_import_and_restart_preserve_source() -> Result<()>
{
    let dir = tempfile::tempdir()?;
    let sessions = dir.path().join("sessions");
    std::fs::create_dir_all(&sessions)?;
    let parent_path = sessions.join("parent.jsonl");
    let mut parent = session(&parent_path)?;
    final_turn(&mut parent, "Original answer")?;
    let parent_id = parent.session_scope_id().to_owned();
    let store = JsonlSessionStore::new(&parent_path)?;
    let records = store.read_event_records_writer()?;
    let fork_point = ConversationForkProjection::from_records(&records)?
        .latest()
        .context("completed turn")?
        .clone();
    let child_path = sessions.join("child.jsonl");
    let fork = fork_conversation_at_turn(
        &store,
        &records,
        &ConversationTurnForkRequest {
            source_turn_digest: fork_point.source_turn_digest,
            source_session_ref: SessionRef::new_relative("parent.jsonl")?,
            destination_path: child_path.clone(),
            provider_name: "test".to_owned(),
            model_name: "test-model".to_owned(),
            resolved_model_route: None,
        },
    )?;
    let mut child = Session::load_from_store_for_control(JsonlSessionStore::new(&child_path)?)?;
    final_turn(
        &mut child,
        "Exploration suggests changing the parser; the claim is not verified",
    )?;
    let service =
        LocalSessionLifecycleService::new("workspace", &sessions, dir.path().join("exports"));
    let parent_ref = SessionRef::new_relative("parent.jsonl")?;
    let child_ref = SessionRef::new_relative("child.jsonl")?;
    let lineage = application_branch_lineage_view(&service, &parent_ref, &parent_id)?;
    assert_eq!(lineage.children.len(), 1);
    assert_eq!(lineage.children[0].session_id, fork.destination_session_id);
    let child_lineage =
        application_branch_lineage_view(&service, &child_ref, child.session_scope_id())?;
    assert_eq!(
        child_lineage.parent.context("parent")?.session_id,
        parent_id
    );
    let preview =
        application_branch_knowledge_preview(&service, &child_ref, child.session_scope_id())?;
    let selected = request(&preview, preview.points.len() - 1);
    let source_before = std::fs::read(&child_path)?;
    let target_before_count = parent.entries().len();
    let receipt = import_application_branch_knowledge(&service, &mut parent, &selected)?;
    assert!(!receipt.already_imported);
    assert_eq!(parent.entries().len(), target_before_count + 1);
    assert!(matches!(
        parent.entries().last(),
        Some(SessionLogEntry::Control(
            ControlEntry::BranchKnowledgeImportedV1(_)
        ))
    ));
    assert_eq!(std::fs::read(&child_path)?, source_before);
    assert!(
        import_application_branch_knowledge(&service, &mut parent, &selected)?.already_imported
    );
    drop(parent);
    drop(store);
    let mut restored = Session::load_from_store_for_control(JsonlSessionStore::new(&parent_path)?)?;
    let repeat = import_application_branch_knowledge(&service, &mut restored, &selected)?;
    assert!(repeat.already_imported);
    assert_eq!(repeat.import_id, receipt.import_id);
    assert_eq!(
        sigil_kernel::branch_knowledge_context(&parent_id, restored.entries())?.items[0]
            .trust_level,
        sigil_kernel::ContextTrustLevel::ExternalUntrusted
    );
    Ok(())
}

#[test]
fn application_branch_knowledge_rejects_forged_selection_and_never_imports_pending_output()
-> Result<()> {
    let dir = tempfile::tempdir()?;
    let source_path = dir.path().join("source.jsonl");
    let mut source = session(&source_path)?;
    final_turn(&mut source, "Completed conclusion")?;
    source.append_user_message(ModelMessage::user("Still working"))?;
    source.append_assistant_message(ModelMessage::assistant(
        Some("Unfinished output".to_owned()),
        Vec::new(),
    ))?;
    let service =
        LocalSessionLifecycleService::new("workspace", dir.path(), dir.path().join("exports"));
    let source_ref = SessionRef::new_relative("source.jsonl")?;
    let preview =
        application_branch_knowledge_preview(&service, &source_ref, source.session_scope_id())?;
    assert_eq!(preview.points.len(), 1);
    assert_eq!(preview.points[0].summary, "Completed conclusion");
    let path = dir.path().join("target.jsonl");
    let mut target = session(&path)?;
    let bytes = std::fs::read(&path)?;
    let mut forged = request(&preview, 0);
    forged.source_text_sha256 = "f".repeat(64);
    assert!(import_application_branch_knowledge(&service, &mut target, &forged).is_err());
    forged = request(&preview, 0);
    forged.summary_sha256 = "e".repeat(64);
    assert!(import_application_branch_knowledge(&service, &mut target, &forged).is_err());
    forged = request(&preview, 0);
    forged.source_session_id = target.session_scope_id().to_owned();
    assert!(import_application_branch_knowledge(&service, &mut target, &forged).is_err());
    assert_eq!(std::fs::read(&path)?, bytes);
    Ok(())
}

#[test]
fn application_branch_knowledge_marks_bounded_excerpt_and_binds_full_source() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut source = session(&dir.path().join("source.jsonl"))?;
    let text = "结论".repeat(DEFAULT_CONTEXT_RENDER_SNIPPET_MAX_BYTES);
    final_turn(&mut source, &text)?;
    let service =
        LocalSessionLifecycleService::new("workspace", dir.path(), dir.path().join("exports"));
    let preview = application_branch_knowledge_preview(
        &service,
        &SessionRef::new_relative("source.jsonl")?,
        source.session_scope_id(),
    )?;
    assert_eq!(preview.points.len(), 1);
    assert!(preview.points[0].truncated);
    assert!(preview.points[0].summary.len() <= DEFAULT_CONTEXT_RENDER_SNIPPET_MAX_BYTES);
    assert_eq!(
        preview.points[0].source_text_sha256,
        stable_event_hash(text.as_bytes())
    );
    Ok(())
}

#[test]
fn application_branch_knowledge_replaced_source_is_rejected_without_target_write() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let source_path = dir.path().join("source.jsonl");
    let mut source = session(&source_path)?;
    final_turn(&mut source, "Original selected conclusion")?;
    let service =
        LocalSessionLifecycleService::new("workspace", dir.path(), dir.path().join("exports"));
    let preview = application_branch_knowledge_preview(
        &service,
        &SessionRef::new_relative("source.jsonl")?,
        source.session_scope_id(),
    )?;
    let selected = request(&preview, 0);
    drop(source);
    let replacement_path = dir.path().join("replacement.jsonl");
    let mut replacement = session(&replacement_path)?;
    final_turn(&mut replacement, "Changed actual source")?;
    drop(replacement);
    std::fs::rename(&replacement_path, &source_path)?;
    let target_path = dir.path().join("target.jsonl");
    let mut target = session(&target_path)?;
    let before = std::fs::read(&target_path)?;
    assert!(import_application_branch_knowledge(&service, &mut target, &selected).is_err());
    assert_eq!(std::fs::read(&target_path)?, before);
    Ok(())
}
