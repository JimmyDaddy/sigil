use super::*;
use crate::app::modal_flow::{SecretInputTarget, TextInputTarget};

#[test]
fn r9_feedback_quit_keeps_full_composer_and_original_modal() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle_paste_text(&format!("{}\n尾行", "草稿".repeat(1_000)));
    app.composer.input_cursor = 6;
    app.composer.input_selection_anchor = Some(12);
    let input = app.composer.input.clone();
    let paste_spans = app.composer.input_paste_spans.clone();
    let attachment = sigil_kernel::ImageAttachment::from_bytes(
        "draft-image",
        sigil_kernel::ImageMimeType::Png,
        1,
        1,
        vec![1, 2, 3],
    )?;
    app.composer.image_attachments.push(attachment.clone());
    app.composer.selected_image_attachment = Some(0);
    app.modal_state = Some(ModalState::InfoDetails { scroll_rows: 3 });

    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))?
            .is_none()
    );
    assert!(app.quit_confirmation_open());
    assert!(app.has_modal());
    assert_eq!(app.modal_title(), Some("Quit Sigil?"));
    assert!(!app.should_quit);
    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(!app.should_quit);
    assert!(!app.quit_confirmation_open());
    assert_eq!(app.composer.input, input);
    assert_eq!(app.composer.input_cursor, 6);
    assert_eq!(app.composer.input_selection_anchor, Some(12));
    assert_eq!(app.composer.input_paste_spans, paste_spans);
    assert_eq!(app.composer.image_attachments, vec![attachment]);
    assert_eq!(app.composer.selected_image_attachment, Some(0));
    assert!(matches!(
        app.modal_state,
        Some(ModalState::InfoDetails { scroll_rows: 3 })
    ));
    Ok(())
}

#[test]
fn r9_feedback_quit_requires_explicit_discard_and_guards_attachment_only_draft() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer
        .image_attachments
        .push(sigil_kernel::ImageAttachment::from_bytes(
            "draft-image",
            sigil_kernel::ImageMimeType::Png,
            1,
            1,
            vec![1, 2, 3],
        )?);
    assert!(app.request_quit().is_none());
    app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))?;
    assert!(!app.should_quit, "repeated Ctrl-C keeps editing");
    assert!(!app.quit_confirmation_open());
    assert!(app.request_quit().is_none());
    app.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))?;
    assert!(app.should_quit);
    Ok(())
}

#[test]
fn r9_feedback_config_quit_preserves_unsaved_settings_and_draft() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input = "  \n".to_owned();
    app.open_config_panel();
    app.config_state.as_mut().expect("config").mark_dirty();
    app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))?;
    assert!(app.quit_confirmation_open());
    assert!(app.is_config_mode());
    assert!(
        app.modal_lines()
            .iter()
            .any(|line| line.contains("unsaved settings"))
    );
    app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    assert!(app.config_state.as_ref().expect("config retained").dirty);
    assert_eq!(app.composer.input, "  \n");
    assert!(!app.should_quit);
    Ok(())
}

#[test]
fn r9_feedback_config_quit_preserves_unapplied_text_and_manual_model_changes() -> Result<()> {
    for target in [
        TextInputTarget::ConfigField(ConfigField::ModelRequestTimeoutSecs),
        TextInputTarget::ConfigManualModel,
    ] {
        for clear in [false, true] {
            let temp = tempdir()?;
            let path = temp.path().join("sigil.toml");
            let mut app = AppState::from_root_config(&path, &test_config());
            app.open_config_panel();
            let config = app.config_state.as_ref().expect("config");
            let original = match target {
                TextInputTarget::ConfigField(field) => config
                    .field_text_value(field)
                    .expect("editable field")
                    .to_owned(),
                TextInputTarget::ConfigManualModel => config.draft.provider_model.clone(),
                _ => unreachable!(),
            };
            assert!(!original.is_empty());
            app.open_text_input(target, &original);
            if clear {
                app.handle_key_event(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL))?;
            } else {
                app.handle_key_event(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE))?;
                app.handle_key_event(KeyEvent::new(KeyCode::Char('7'), KeyModifiers::NONE))?;
                app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT))?;
            }
            let modal = format!("{:?}", app.modal_state);
            let config = format!("{:?}", app.config_state);
            app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))?;
            assert!(app.quit_confirmation_open());
            assert!(!app.should_quit);
            assert!(
                !app.config_is_dirty(),
                "navigation must not apply the input"
            );
            assert!(app.modal_lines().join("\n").contains("unsaved settings"));
            app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
            assert_eq!(format!("{:?}", app.modal_state), modal);
            assert_eq!(format!("{:?}", app.config_state), config);
            assert!(!path.exists());
            if clear {
                assert!(
                    app.modal_input_view()
                        .expect("restored editor")
                        .value
                        .is_empty()
                );
            }
            app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))?;
            app.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))?;
            assert!(app.should_quit);
            assert!(
                !path.exists(),
                "discard must not publish an incomplete value"
            );
        }
    }
    Ok(())
}

#[test]
fn r9_feedback_config_quit_keeps_unapplied_secret_without_projecting_its_value() -> Result<()> {
    for clear in [false, true] {
        let temp = tempdir()?;
        let path = temp.path().join("sigil.toml");
        let mut app = AppState::from_root_config(&path, &test_config());
        app.open_config_panel();
        let original = if clear { "fixture-original-secret" } else { "" };
        app.config_state
            .as_mut()
            .expect("config")
            .draft
            .provider_api_key = SecretString::new(original);
        app.open_secret_input(SecretInputTarget::ConfigProviderApiKey, original);
        if clear {
            app.handle_key_event(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL))?;
        } else {
            app.handle_paste_text("fixture-replacement-secret");
            app.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT))?;
        }
        let expected = if clear {
            ""
        } else {
            "fixture-replacement-secret"
        };
        let projected = app.modal_input_view().expect("masked editor");
        app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))?;
        assert!(app.quit_confirmation_open());
        assert!(!app.should_quit);
        assert!(!app.config_is_dirty());
        assert!(!app.modal_lines().join("\n").contains("fixture-"));
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
        let Some(ModalState::SecretInput(input)) = app.modal_state.as_ref() else {
            panic!("secret editor must survive keeping the draft");
        };
        assert_eq!(input.buffer.expose_secret(), expected);
        assert_eq!(input.cursor.position, projected.cursor);
        assert_eq!(input.cursor.selection(), projected.selection);
        assert_eq!(
            app.config_state
                .as_ref()
                .expect("config")
                .draft
                .provider_api_key
                .expose_secret(),
            original
        );
        assert!(!path.exists());
    }
    Ok(())
}

#[test]
fn r9_feedback_config_quit_does_not_warn_for_unchanged_prefilled_editors() -> Result<()> {
    for target in [
        TextInputTarget::ConfigField(ConfigField::ModelRequestTimeoutSecs),
        TextInputTarget::ConfigManualModel,
    ] {
        let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
        app.open_config_panel();
        let config = app.config_state.as_ref().expect("config");
        let original = match target {
            TextInputTarget::ConfigField(field) => config
                .field_text_value(field)
                .expect("editable field")
                .to_owned(),
            TextInputTarget::ConfigManualModel => config.draft.provider_model.clone(),
            _ => unreachable!(),
        };
        app.open_text_input(target, &original);
        app.handle_key_event(KeyEvent::new(KeyCode::Char('7'), KeyModifiers::NONE))?;
        app.handle_key_event(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL))?;
        app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))?;
        assert!(
            app.should_quit,
            "an undone edit must not add an exit warning"
        );
        assert!(!app.quit_confirmation_open());
    }
    for original in ["", "fixture-original-secret"] {
        let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
        app.open_config_panel();
        app.config_state
            .as_mut()
            .expect("config")
            .draft
            .provider_api_key = SecretString::new(original);
        app.open_secret_input(SecretInputTarget::ConfigProviderApiKey, original);
        app.handle_key_event(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE))?;
        app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))?;
        assert!(
            app.should_quit,
            "prefill and caret movement are not changes"
        );
        assert!(!app.quit_confirmation_open());
    }
    Ok(())
}

#[test]
fn r9_feedback_quit_still_cancels_busy_run_and_ignores_command_token() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input = "unsent".to_owned();
    app.runtime.is_busy = true;
    assert!(matches!(
        app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))?,
        Some(AppAction::CancelRun)
    ));
    assert!(!app.should_quit);
    assert!(!app.quit_confirmation_open());
    assert_eq!(app.composer.input, "unsent");
    app.runtime.is_busy = false;
    assert!(app.request_quit().is_none());
    app.runtime.is_busy = true;
    assert!(matches!(
        app.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))?,
        Some(AppAction::CancelRun)
    ));
    assert!(
        !app.should_quit,
        "promotion while confirming must not bypass cancellation"
    );
    app.runtime.is_busy = false;
    app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    app.composer.input = "/quit".to_owned();
    assert!(app.submit_input()?.is_none());
    assert!(app.should_quit);
    assert!(!app.quit_confirmation_open());
    Ok(())
}
