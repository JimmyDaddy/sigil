use super::*;
use sigil_kernel::{
    MutationEventRecorder, ToolDiffBudget, ToolPreviewFile, write_file_with_mutation,
};

fn fixture() -> Result<(tempfile::TempDir, AppState)> {
    let temp = tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let mut config = test_config();
    config.workspace.root = workspace.display().to_string();
    config.storage.state_root =
        sigil_kernel::StorageRoot::Path(temp.path().join("state").display().to_string());
    config.storage.cache_root =
        sigil_kernel::StorageRoot::Path(temp.path().join("cache").display().to_string());
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    store.append(&SessionLogEntry::User(ModelMessage::user("create a note")))?;
    let recorder =
        MutationEventRecorder::with_artifact_root(store.clone(), temp.path().join("artifacts"));
    let preview = ToolPreviewSnapshot::from_preview(
        "write-note",
        "write_file",
        &ToolPreview {
            title: "Create note".to_owned(),
            summary: "Recorded creation".to_owned(),
            body: String::new(),
            changed_files: vec!["note.txt".to_owned()],
            file_diffs: vec![ToolPreviewFile {
                path: "note.txt".to_owned(),
                diff: "--- /dev/null\n+++ note.txt\n@@ -0,0 +1,2 @@\n+first\n+second\n".to_owned(),
            }],
        },
        ToolDiffBudget::default(),
        None,
    );
    store.append(&SessionLogEntry::Control(
        ControlEntry::ToolPreviewCaptured(preview),
    ))?;
    write_file_with_mutation(
        Some(&recorder),
        &workspace,
        "write-note",
        "note.txt",
        workspace.join("note.txt"),
        b"first\nsecond\n",
    )?;
    let scope = store.read_event_records_writer()?[0]
        .session_id()
        .to_owned();
    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);
    app.session_id = scope;
    app.session_log_path = path;
    app.composer.input = "Keep my original draft".to_owned();
    Ok((temp, app))
}

fn settle_read(app: &mut AppState) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while app.change_review_task.is_some() {
        app.poll_change_review();
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "review read did not finish"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    Ok(())
}

fn select_and_comment(app: &mut AppState) -> Result<()> {
    app.handle_key_event(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::ALT))?;
    settle_read(app)?;
    assert_eq!(app.modal_title(), Some("Review Recorded Changes"));
    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    settle_read(app)?;
    for _ in 0..3 {
        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    }
    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let outcome = app.handle_modal_paste_text("Please clarify both lines");
    app.apply_modal_outcome(outcome);
    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL))?;
    assert!(app.modal_lines()[0].contains("1 comments"));
    Ok(())
}

#[test]
fn change_review_real_diff_changed_file_can_send_and_queue_without_losing_draft() -> Result<()> {
    let (_temp, mut app) = fixture()?;
    std::fs::write(
        app.workspace_root.join("note.txt"),
        "later unrelated file content",
    )?;
    select_and_comment(&mut app)?;
    assert!(
        app.modal_lines()
            .iter()
            .any(|line| line.contains("Changed now"))
    );
    let source = std::fs::read(&app.session_log_path)?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;
    settle_read(&mut app)?;
    let action = app.take_change_review_submission().expect("review submit");
    let AppAction::SubmitPrompt(prompt) = &action else {
        anyhow::bail!("expected normal submit")
    };
    assert!(prompt.contains("Please clarify both lines"));
    assert!(prompt.contains("path=note.txt side=New lines=1-2"));
    assert!(prompt.contains("1: +first"));
    assert!(prompt.contains("current file content unknown at execution"));
    assert_eq!(app.composer.input, "Keep my original draft");
    assert_eq!(
        std::fs::read(&app.session_log_path)?,
        source,
        "review itself does not mutate history"
    );
    assert!(app.settle_change_review_submission(&action, Some("temporary admission failure")));
    assert!(
        app.modal_lines()
            .iter()
            .any(|line| line.contains("temporary admission failure"))
    );
    app.runtime.is_busy = true;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;
    settle_read(&mut app)?;
    let queued = app.take_change_review_submission().expect("review queue");
    assert!(
        matches!(&queued, AppAction::QueueConversationInput { prompt: queued, .. } if queued == prompt),
        "retry produces the same exact queued context"
    );
    assert!(app.settle_change_review_submission(&queued, None));
    assert!(!app.change_review_modal_open());
    assert_eq!(app.composer.input, "Keep my original draft");
    assert_eq!(
        std::fs::read_to_string(app.workspace_root.join("note.txt"))?,
        "later unrelated file content"
    );
    Ok(())
}

#[test]
fn change_review_async_materialization_cannot_send_after_close_or_session_change() -> Result<()> {
    let (_temp, mut app) = fixture()?;
    select_and_comment(&mut app)?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    settle_read(&mut app)?;
    assert!(app.take_change_review_submission().is_none());
    select_and_comment(&mut app)?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;
    app.session_id = "another-session".to_owned();
    settle_read(&mut app)?;
    assert!(app.take_change_review_submission().is_none());
    assert_eq!(app.composer.input, "Keep my original draft");
    assert!(!app.runtime.is_busy);
    Ok(())
}

#[test]
fn change_review_missing_old_line_does_not_invent_a_source_reference() -> Result<()> {
    let (_temp, mut app) = fixture()?;
    app.open_change_review();
    settle_read(&mut app)?;
    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    settle_read(&mut app)?;
    for _ in 0..3 {
        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    }
    app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    app.paste_change_review_comment("This old line does not exist");
    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL))?;
    assert!(
        app.modal_lines()
            .iter()
            .any(|line| line.contains("selected side has no source line"))
    );
    assert!(app.modal_lines()[0].contains("0 comments"));
    assert_eq!(app.composer.input, "Keep my original draft");
    Ok(())
}

#[test]
fn change_review_run_receipt_requires_exact_prompt_and_current_submission_intent() -> Result<()> {
    let (_temp, mut app) = fixture()?;
    select_and_comment(&mut app)?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;
    settle_read(&mut app)?;
    let AppAction::SubmitPrompt(prompt) = app.take_change_review_submission().expect("submit")
    else {
        anyhow::bail!("normal submit")
    };
    let intent = std::sync::Arc::clone(&app.runtime.run_submission_intent);
    app.observe_change_review_run_message(&WorkerMessage::RunStarted {
        prompt: "another prompt".to_owned(),
    });
    assert!(app.change_review_modal_open());
    app.begin_run_submission_intent();
    app.observe_change_review_run_message(&WorkerMessage::RunFailed(
        "late prior failure".to_owned(),
    ));
    assert!(
        !app.modal_lines()
            .iter()
            .any(|line| line.contains("late prior failure"))
    );
    app.runtime.run_submission_intent = intent;
    app.observe_change_review_run_message(&WorkerMessage::RunStarted {
        prompt: sigil_kernel::safe_persistence_text(&prompt),
    });
    assert!(!app.change_review_modal_open());
    assert_eq!(app.composer.input, "Keep my original draft");
    Ok(())
}
