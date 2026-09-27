use super::*;
use sigil_runtime::application_branch_knowledge::{
    ApplicationBranchKnowledgeImportReceipt, ApplicationBranchKnowledgePointView,
    ApplicationBranchKnowledgePreview, ApplicationBranchLineageView,
};

fn preview() -> Result<ApplicationBranchKnowledgePreview> {
    Ok(ApplicationBranchKnowledgePreview {
        source_session_ref: sigil_kernel::SessionRef::new_relative("branch.jsonl")?,
        source_session_id: "exploration-branch".to_owned(),
        points: vec![ApplicationBranchKnowledgePointView {
            source_turn_digest: "a".repeat(64),
            source_message_id: "final-message".to_owned(),
            source_text_sha256: "b".repeat(64),
            summary_sha256: "c".repeat(64),
            summary: "An unverified branch conclusion".to_owned(),
            truncated: true,
        }],
    })
}

fn load(app: &mut AppState) -> Result<u64> {
    let preview = preview()?;
    let AppAction::LoadBranchKnowledge {
        request_id,
        target_session_id,
        ..
    } = app.open_branch_knowledge_modal(
        preview.source_session_ref.clone(),
        preview.source_session_id.clone(),
    )
    else {
        anyhow::bail!("expected source query")
    };
    app.handle_worker_message(WorkerMessage::BranchKnowledgeLoaded {
        request_id,
        target_session_id,
        lineage: ApplicationBranchLineageView {
            session_id: preview.source_session_id.clone(),
            parent: None,
            children: Vec::new(),
            unavailable_count: 0,
        },
        preview,
    })?;
    Ok(request_id)
}

#[test]
fn branch_knowledge_selection_imports_exact_excerpt_without_submitting_or_changing_draft()
-> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input = "My next question stays editable".to_owned();
    let request_id = load(&mut app)?;
    assert_eq!(app.modal_title(), Some("Bring Back a Conclusion"));
    assert!(
        app.modal_lines()
            .iter()
            .any(|line| line.contains("only this excerpt"))
    );
    let Some(AppAction::ImportBranchKnowledge {
        request_id: selected_id,
        target_session_id,
        request,
    }) = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?
    else {
        anyhow::bail!("expected explicit knowledge import, not prompt submission")
    };
    assert_eq!(selected_id, request_id);
    assert_eq!(target_session_id, app.session_id);
    assert_eq!(request.source_turn_digest, "a".repeat(64));
    assert_eq!(request.source_message_id, "final-message");
    assert_eq!(request.source_text_sha256, "b".repeat(64));
    assert_eq!(request.summary_sha256, "c".repeat(64));
    assert!(app.branch_knowledge_applying());
    assert!(app.apply_branch_knowledge_receipt(
        request_id,
        &target_session_id,
        &ApplicationBranchKnowledgeImportReceipt {
            import_id: "imported".to_owned(),
            already_imported: false
        }
    ));
    assert!(!app.branch_knowledge_modal_open());
    assert_eq!(app.composer.input, "My next question stays editable");
    assert!(!app.runtime.is_busy);
    assert!(
        app.last_notice
            .as_deref()
            .is_some_and(|notice| notice.contains("unverified knowledge"))
    );
    Ok(())
}

#[test]
fn branch_knowledge_ignores_closed_or_foreign_receipts_and_preserves_failed_selection() -> Result<()>
{
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input = "draft".to_owned();
    let old_request = load(&mut app)?;
    let target = app.session_id.clone();
    app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    let request_id = load(&mut app)?;
    let receipt = ApplicationBranchKnowledgeImportReceipt {
        import_id: "old-import".to_owned(),
        already_imported: false,
    };
    assert!(!app.apply_branch_knowledge_receipt(old_request, &target, &receipt));
    assert!(!app.apply_branch_knowledge_error(old_request, "late failure"));
    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(!app.apply_branch_knowledge_receipt(request_id, "foreign-target", &receipt));
    assert!(app.apply_branch_knowledge_error(request_id, "source changed; refresh"));
    assert!(!app.branch_knowledge_applying());
    assert!(
        app.modal_lines()
            .iter()
            .any(|line| line.contains("source changed"))
    );
    assert_eq!(app.composer.input, "draft");
    app.session_id = "replacement-target".to_owned();
    assert!(!app.apply_branch_knowledge_receipt(request_id, &target, &receipt));
    assert!(!app.apply_branch_knowledge_error(request_id, "old target failure"));
    Ok(())
}
