use std::fs;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sigil_kernel::{ModelMessage, SessionLogEntry};
use sigil_runtime::support::{SUPPORT_BUNDLES_DIRECTORY_NAME, SupportBuildInfo, SupportBundleV1};
use tempfile::tempdir;

use super::super::tests::common::test_config;
use super::*;

fn feedback_app() -> anyhow::Result<AppState> {
    let temp = tempdir()?;
    let workspace = temp.keep();
    let config_path = workspace.join("sigil.toml");
    let mut config = test_config();
    config.workspace.root = workspace.display().to_string();
    config.save(&config_path)?;
    let mut app = AppState::from_root_config(&config_path, &config);
    app.set_support_build_info(SupportBuildInfo::new(
        "0.0.1-alpha.3",
        "feedback-test-commit",
        "aarch64-apple-darwin",
        "test",
    ));
    Ok(app)
}

fn open_feedback(app: &mut AppState) -> anyhow::Result<()> {
    app.composer.input = "/feedback".to_owned();
    assert!(app.submit_input()?.is_none());
    Ok(())
}

#[test]
fn feedback_preview_is_private_and_writes_nothing_before_enter() -> anyhow::Result<()> {
    let mut app = feedback_app()?;
    let timeline_count = app.timeline.len();
    let event_count = app.events.len();
    let durable_entry_count = app.session_browser.current_entries.len();
    let support_dir = app
        .sigil_paths
        .cache_root
        .join(SUPPORT_BUNDLES_DIRECTORY_NAME);

    open_feedback(&mut app)?;

    assert_eq!(app.modal_title(), Some("Feedback Report"));
    let lines = app.modal_lines().join("\n");
    assert!(lines.contains("Nothing has been written or uploaded"));
    assert!(lines.contains("Included: build, OS/architecture"));
    assert!(lines.contains("Excluded: conversation, tool input/output, file content/diff"));
    assert!(lines.contains("Metadata may include provider/model labels"));
    assert!(lines.contains("Enter export locally  Esc cancel"));
    assert!(!support_dir.exists());
    assert_eq!(app.timeline.len(), timeline_count);
    assert_eq!(app.events.len(), event_count);
    assert_eq!(
        app.session_browser.current_entries.len(),
        durable_entry_count
    );
    assert!(!app.session_log_path.exists());
    Ok(())
}

#[test]
fn feedback_modal_owns_input_and_exports_only_redacted_coarse_facts() -> anyhow::Result<()> {
    let canaries = [
        "PRIVATE-PROMPT-CANARY",
        "PRIVATE-ASSISTANT-CANARY",
        "PRIVATE-TOOL-CANARY",
    ];
    let mut app = feedback_app()?;
    app.session_browser
        .current_entries
        .push(SessionLogEntry::User(ModelMessage::user(canaries[0])));
    app.session_browser
        .current_entries
        .push(SessionLogEntry::Assistant(ModelMessage::assistant(
            Some(canaries[1].to_owned()),
            Vec::new(),
        )));
    app.session_browser
        .current_entries
        .push(SessionLogEntry::ToolResult(ModelMessage::tool(
            "private-call",
            canaries[2],
        )));
    let durable_entry_count = app.session_browser.current_entries.len();
    let timeline_count = app.timeline.len();
    let event_count = app.events.len();
    open_feedback(&mut app)?;
    app.composer.input = "composer draft".to_owned();

    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE))?
            .is_none()
    );
    assert_eq!(app.composer.input, "composer draft");
    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Char('\r'), KeyModifiers::NONE))?
            .is_none()
    );
    assert_eq!(app.composer.input, "composer draft");

    let exported_path = match app.modal_state.as_ref() {
        Some(ModalState::Feedback(state)) => state
            .exported_path
            .clone()
            .expect("feedback report should be exported"),
        _ => panic!("feedback modal should remain open after export"),
    };
    let json = fs::read_to_string(&exported_path)?;
    for canary in canaries {
        assert!(!json.contains(canary), "session content leaked: {canary}");
    }
    let bundle: SupportBundleV1 = serde_json::from_str(&json)?;
    assert_eq!(bundle.schema_version, 1);
    assert_eq!(bundle.doctor.build.commit, "feedback-test-commit");
    assert_eq!(
        bundle
            .session
            .as_ref()
            .expect("session")
            .durable_entry_count(),
        durable_entry_count
    );
    assert_eq!(app.timeline.len(), timeline_count);
    assert_eq!(app.events.len(), event_count);
    assert_eq!(
        app.session_browser.current_entries.len(),
        durable_entry_count
    );
    assert!(!app.session_log_path.exists());

    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?
            .is_none()
    );
    assert_eq!(
        fs::read_dir(exported_path.parent().expect("support directory"))?.count(),
        1
    );
    let copy = app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE))?;
    assert!(matches!(
        copy,
        Some(AppAction::CopyToClipboard { text }) if text == GITHUB_BUG_REPORT_URL
    ));
    assert_eq!(app.composer.input, "composer draft");
    app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    assert!(!app.has_modal());
    Ok(())
}

#[test]
fn feedback_export_accepts_terminal_newline_key_code() -> anyhow::Result<()> {
    let mut app = feedback_app()?;
    open_feedback(&mut app)?;

    app.handle_key_event(KeyEvent::new(KeyCode::Char('\n'), KeyModifiers::NONE))?;

    assert!(matches!(
        app.modal_state,
        Some(ModalState::Feedback(ref state)) if state.exported_path.is_some()
    ));
    Ok(())
}

#[cfg(unix)]
#[test]
fn feedback_export_failure_stays_in_modal_and_can_be_cancelled() -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;

    let mut app = feedback_app()?;
    open_feedback(&mut app)?;
    let external = app.workspace_root.join("external-support");
    fs::create_dir_all(&external)?;
    fs::create_dir_all(&app.sigil_paths.cache_root)?;
    symlink(
        &external,
        app.sigil_paths
            .cache_root
            .join(SUPPORT_BUNDLES_DIRECTORY_NAME),
    )?;

    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(app.feedback_modal_open());
    assert!(app.modal_lines().join("\n").contains("Export failed:"));
    assert_eq!(fs::read_dir(&external)?.count(), 0);
    app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    assert!(!app.has_modal());
    Ok(())
}
