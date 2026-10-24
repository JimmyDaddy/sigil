use std::path::Path;

use anyhow::Result;

use super::*;
use crate::app::tests::common::test_config;

fn revision_request() -> Result<sigil_kernel::PublicUserInputRequestV1> {
    Ok(sigil_kernel::PublicUserInputRequestV1 {
        identity: sigil_kernel::UserInputIdentityV1 {
            session_scope_id: sigil_kernel::SessionScopeId::new("revision-editor-session")?,
            root_logical_run_id: sigil_kernel::LogicalRunId::new("revision-editor-run")?,
            source_thread_id: sigil_kernel::AgentThreadId::new("main")?,
            request_id: sigil_kernel::UserInputRequestId::new("revision-editor-request")?,
            generation: 1,
            source_binding_hash: format!("sha256:{}", "a".repeat(64)),
        },
        request_hash: format!("sha256:{}", "b".repeat(64)),
        source: sigil_kernel::UserInputSourceV1::PlanRevision {
            base_plan_id: sigil_kernel::PlanId::new("revision-editor-plan")?,
            base_plan_hash: format!("sha256:{}", "c".repeat(64)),
        },
        purpose: sigil_kernel::UserInputPurposeV1::RevisionGuidance,
        prompt: "What should change in this plan?".to_owned(),
        prompt_origin: None,
        questions: vec![sigil_kernel::UserInputQuestionV1 {
            id: "revision_guidance".to_owned(),
            question: "Describe the change.".to_owned(),
            description: None,
            required: true,
            options: Vec::new(),
            multiple: false,
        }],
        allowed_actions: vec![
            sigil_kernel::UserInputActionV1::Submit,
            sigil_kernel::UserInputActionV1::Decline,
        ],
        requested_at_unix_ms: 1,
        status: sigil_kernel::UserInputStatusV1::Requested,
        answer_receipt: None,
        resolution: None,
    })
}

fn revision_app() -> Result<AppState> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_pending_user_input(revision_request()?);
    Ok(app)
}

fn press(app: &mut AppState, code: KeyCode, modifiers: KeyModifiers) -> Result<Option<AppAction>> {
    app.handle_key_event(KeyEvent::new(code, modifiers))
}

fn text(app: &AppState) -> &str {
    match &app.pending_user_input().expect("revision form").drafts[0] {
        UserInputDraftValue::Text(value) => value,
        _ => panic!("revision request must remain text"),
    }
}

#[test]
fn plan_revision_enter_submits_once_without_an_action_picker() -> Result<()> {
    let mut app = revision_app()?;
    let request = revision_request()?;
    app.handle_paste_text("keep this constraint");
    let action = press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?;
    assert!(matches!(
        action,
        Some(AppAction::SubmitUserInputDecision {
            command_id: None,
            request_id,
            generation: 1,
            expected_request_hash,
            decision: sigil_kernel::UserInputDecisionV1::Submitted { answers },
        }) if request_id == request.identity.request_id.as_str()
            && expected_request_hash == request.request_hash
            && answers == vec![sigil_kernel::UserInputAnswerV1 {
                question_id: "revision_guidance".to_owned(),
                value: sigil_kernel::UserInputAnswerValueV1::Text {
                    value: "keep this constraint".to_owned(),
                },
            }]
    ));
    assert!(
        app.pending_user_input()
            .expect("form")
            .plan_revision_editor
            .submitting
    );
    assert!(!app.pending_user_input().expect("form").focus_actions);
    assert!(press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?.is_none());
    app.handle_paste_text("must not replace the submitted payload");
    assert_eq!(text(&app), "keep this constraint");
    Ok(())
}

#[test]
fn plan_revision_shift_enter_control_j_and_multiline_paste_only_edit() -> Result<()> {
    let mut app = revision_app()?;
    app.handle_paste_text("first");
    assert!(press(&mut app, KeyCode::Enter, KeyModifiers::SHIFT)?.is_none());
    app.handle_paste_text("second");
    assert!(press(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL)?.is_none());
    app.handle_paste_text("third\r\npasted\nline");

    assert_eq!(text(&app), "first\nsecond\nthird\npasted\nline");
    assert!(app.composer.input.is_empty());
    assert!(
        !app.pending_user_input()
            .expect("form")
            .plan_revision_editor
            .submitting
    );
    Ok(())
}

#[test]
fn plan_revision_keys_edit_at_the_cursor_and_open_with_editor_focus() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.active_pane = crate::app::PaneFocus::Activity;
    app.set_pending_user_input(revision_request()?);
    assert_eq!(app.active_pane, crate::app::PaneFocus::Composer);
    assert!(!app.pending_user_input().expect("form").focus_actions);
    app.handle_paste_text("first\nsecond");
    press(&mut app, KeyCode::Home, KeyModifiers::NONE)?;
    press(&mut app, KeyCode::Char('X'), KeyModifiers::SHIFT)?;
    press(&mut app, KeyCode::Right, KeyModifiers::NONE)?;
    press(&mut app, KeyCode::Delete, KeyModifiers::NONE)?;
    press(&mut app, KeyCode::Backspace, KeyModifiers::NONE)?;
    assert_eq!(text(&app), "first\nXcond");
    press(&mut app, KeyCode::Up, KeyModifiers::NONE)?;
    press(&mut app, KeyCode::End, KeyModifiers::NONE)?;
    press(&mut app, KeyCode::Char('!'), KeyModifiers::NONE)?;
    assert_eq!(text(&app), "first!\nXcond");
    Ok(())
}

#[test]
fn plan_revision_escape_returns_and_reopens_the_same_draft() -> Result<()> {
    let mut app = revision_app()?;
    app.handle_paste_text("keep this\nexact text");
    press(&mut app, KeyCode::Left, KeyModifiers::NONE)?;
    let cursor = app
        .pending_user_input()
        .expect("form")
        .plan_revision_editor
        .cursor;
    assert!(press(&mut app, KeyCode::Esc, KeyModifiers::NONE)?.is_none());
    assert!(!app.pending_user_input().expect("form").open);
    assert!(press(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT)?.is_none());
    assert!(app.pending_user_input().expect("form").open);
    assert_eq!(text(&app), "keep this\nexact text");
    assert_eq!(
        app.pending_user_input()
            .expect("form")
            .plan_revision_editor
            .cursor,
        cursor
    );
    assert!(!app.pending_user_input().expect("form").focus_actions);
    Ok(())
}

#[test]
fn plan_revision_validation_and_exact_submission_failure_keep_the_text_inline() -> Result<()> {
    let mut app = revision_app()?;
    let request = revision_request()?;
    assert!(press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?.is_none());
    assert_eq!(
        app.pending_user_input()
            .expect("form")
            .plan_revision_editor
            .error
            .as_deref(),
        Some("Revision request requires an answer")
    );
    app.handle_paste_text("unchanged answer");
    assert!(press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?.is_some());
    assert!(!app.fail_pending_user_input_submission(
        request.identity.request_id.as_str(),
        2,
        &request.request_hash,
        "old request failed".to_owned(),
    ));
    assert!(
        app.pending_user_input()
            .expect("form")
            .plan_revision_editor
            .submitting
    );
    assert!(app.fail_pending_user_input_submission(
        request.identity.request_id.as_str(),
        1,
        &request.request_hash,
        "Could not submit; retry".to_owned(),
    ));
    assert_eq!(text(&app), "unchanged answer");
    let form = app.pending_user_input().expect("form");
    assert!(form.open);
    assert!(!form.plan_revision_editor.submitting);
    assert_eq!(
        form.plan_revision_editor.error.as_deref(),
        Some("Could not submit; retry")
    );
    assert!(press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?.is_some());
    Ok(())
}

#[test]
fn plan_revision_reprojection_preserves_only_the_same_pending_request() -> Result<()> {
    let mut app = revision_app()?;
    let request = revision_request()?;
    app.handle_paste_text("retain across projection");
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?;
    let previous = app.pending_user_input().cloned();
    app.clear_pending_user_input();
    app.set_pending_user_input(request.clone());
    app.restore_pending_user_input_presentation(previous.clone());
    assert_eq!(text(&app), "retain across projection");
    assert!(
        app.pending_user_input()
            .expect("form")
            .plan_revision_editor
            .submitting
    );

    app.clear_pending_user_input();
    let mut changed = request;
    changed.request_hash = format!("sha256:{}", "d".repeat(64));
    app.set_pending_user_input(changed);
    app.restore_pending_user_input_presentation(previous.clone());
    assert_eq!(text(&app), "");
    assert!(
        !app.pending_user_input()
            .expect("form")
            .plan_revision_editor
            .submitting
    );

    app.clear_pending_user_input();
    app.restore_pending_user_input_presentation(previous);
    assert!(
        app.pending_user_input().is_none(),
        "resolved requests cannot return"
    );
    Ok(())
}

#[test]
fn plan_revision_duplicate_request_preserves_a_closed_draft() -> Result<()> {
    let mut app = revision_app()?;
    app.handle_paste_text("retained");
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE)?;
    app.set_pending_user_input(revision_request()?);
    assert_eq!(text(&app), "retained");
    assert!(!app.pending_user_input().expect("form").open);
    Ok(())
}

#[test]
fn ordinary_text_form_uses_enter_to_focus_actions() -> Result<()> {
    let mut app = revision_app()?;
    app.clear_pending_user_input();
    let mut request = revision_request()?;
    request.source = sigil_kernel::UserInputSourceV1::Agent;
    request.purpose = sigil_kernel::UserInputPurposeV1::Clarification;
    app.set_pending_user_input(request);
    press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE)?;
    assert!(press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?.is_none());
    assert!(app.pending_user_input().expect("form").focus_actions);
    assert!(press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?.is_some());
    assert!(
        !app.pending_user_input()
            .expect("form")
            .plan_revision_editor
            .submitting
    );
    Ok(())
}

fn ordinary_app_with_questions(
    questions: Vec<sigil_kernel::UserInputQuestionV1>,
) -> Result<AppState> {
    let mut request = revision_request()?;
    request.source = sigil_kernel::UserInputSourceV1::Agent;
    request.purpose = sigil_kernel::UserInputPurposeV1::Clarification;
    request.questions = questions;
    request
        .allowed_actions
        .push(sigil_kernel::UserInputActionV1::CancelRun);
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_pending_user_input(request);
    Ok(app)
}

fn ordinary_question(id: &str) -> sigil_kernel::UserInputQuestionV1 {
    sigil_kernel::UserInputQuestionV1 {
        id: id.to_owned(),
        question: format!("请回答 {id}？"),
        description: None,
        required: true,
        options: Vec::new(),
        multiple: false,
    }
}

fn choice_question(multiple: bool) -> sigil_kernel::UserInputQuestionV1 {
    let mut question = ordinary_question("choice");
    question.options = vec![sigil_kernel::UserInputOptionV1 {
        id: "suggested".to_owned(),
        label: "建议选项".to_owned(),
        description: None,
    }];
    question.multiple = multiple;
    question
}

fn host_question_group_app() -> Result<AppState> {
    let mut single = choice_question(false);
    single.id = "priority".to_owned();
    single.question = "先检查哪个功能？".to_owned();
    single.options[0].label = "输入体验".to_owned();
    single.options.push(sigil_kernel::UserInputOptionV1 {
        id: "recovery".to_owned(),
        label: "恢复体验".to_owned(),
        description: None,
    });
    let mut multiple = single.clone();
    multiple.id = "surfaces".to_owned();
    multiple.question = "检查哪些界面？".to_owned();
    multiple.multiple = true;
    multiple.options[0].label = "TUI".to_owned();
    multiple.options[1].label = "Desktop".to_owned();
    let mut detail = ordinary_question("detail");
    detail.question = "还有哪些要求？".to_owned();
    let mut app = ordinary_app_with_questions(vec![single, multiple, detail])?;
    let form = app.composer.pending_user_input.as_mut().expect("form");
    form.view.prompt_origin = Some(sigil_kernel::UserInputPromptOriginV1::HostQuestionGroup);
    form.request.as_mut().expect("request").prompt_origin = form.view.prompt_origin;
    Ok(app)
}

fn ordinary_surface_text(app: &AppState, height: u16) -> Result<String> {
    let screen = ratatui::layout::Rect::new(0, 0, 80, height);
    let surface = crate::surface_adapter::build_surface_model(
        screen,
        app,
        crate::surface::SurfaceState {
            frame_generation: 1,
            terminal_epoch: 1,
        },
    );
    if surface.user_input_open {
        assert_eq!(surface.layout.composer.height, 0);
        assert_eq!(surface.layout.agent_panel.height, 0);
        assert_eq!(surface.layout.live_panel.height, height - 1);
        assert!(surface.layout.egress_disclosure.is_none());
    }
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, height))?;
    terminal.draw(|frame| crate::ui::render_surface(frame, &surface))?;
    let buffer = terminal.backend().buffer();
    let mut rendered = String::new();
    for row in buffer.content().chunks(usize::from(buffer.area.width)) {
        let mut column = 0;
        while let Some(cell) = row.get(column) {
            rendered.push_str(cell.symbol());
            column += crate::ui::terminal_cell_width(cell.symbol()).max(1);
        }
        rendered.push('\n');
    }
    Ok(rendered)
}

#[test]
fn ordinary_form_production_surface_keeps_three_questions_reachable_on_short_terminals()
-> Result<()> {
    for height in [12, 24] {
        let mut app = host_question_group_app()?;
        app.set_terminal_size(80, height);
        app.composer.input = "原始草稿\n保留选区".to_owned();
        app.composer.input_cursor = 3;
        app.composer.input_selection_anchor = Some(1);
        let rendered = ordinary_surface_text(&app, height)?;
        assert!(
            rendered.contains("先检查哪个功能？"),
            "{height}: {rendered}"
        );
        assert!(rendered.contains("输入体验"));
        assert!(!rendered.contains("请回答以下问题后继续"));
        assert!(!rendered.contains("What should change in this plan?"));
        press(&mut app, KeyCode::PageDown, KeyModifiers::NONE)?;
        assert!(ordinary_surface_text(&app, height)?.contains("还有哪些要求？"));
        press(&mut app, KeyCode::PageUp, KeyModifiers::NONE)?;
        assert!(ordinary_surface_text(&app, height)?.contains("先检查哪个功能？"));

        press(&mut app, KeyCode::Enter, KeyModifiers::CONTROL)?;
        assert!(press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?.is_none());
        let rendered = ordinary_surface_text(&app, height)?;
        assert!(rendered.contains("Please answer question 1 before submitting"));
        assert!(rendered.contains("先检查哪个功能？"));
        assert!(rendered.contains("Submit"));
        press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE)?;
        press(&mut app, KeyCode::Tab, KeyModifiers::NONE)?;
        let rendered = ordinary_surface_text(&app, height)?;
        assert!(rendered.contains("检查哪些界面？"));
        assert!(rendered.contains("Desktop"));
        press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE)?;
        press(&mut app, KeyCode::Tab, KeyModifiers::NONE)?;
        let rendered = ordinary_surface_text(&app, height)?;
        assert!(rendered.contains("还有哪些要求？"));
        assert!(rendered.contains("type an answer"));
        app.handle_paste_text("简洁明了");
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE)?;
        assert!(!app.pending_user_input().expect("form").open);
        let surface = crate::surface_adapter::build_surface_model(
            ratatui::layout::Rect::new(0, 0, 80, height),
            &app,
            crate::surface::SurfaceState {
                frame_generation: 2,
                terminal_epoch: 1,
            },
        );
        assert!(surface.layout.composer.height > 0);
        press(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT)?;
        let rendered = ordinary_surface_text(&app, height)?;
        assert!(rendered.contains("还有哪些要求？"));
        assert!(rendered.contains("简洁明了"));
        assert_eq!(app.composer.input, "原始草稿\n保留选区");
        assert_eq!(app.composer.input_cursor, 3);
        assert_eq!(app.composer.input_selection_anchor, Some(1));
    }
    Ok(())
}

#[test]
fn ordinary_form_other_space_edits_without_changing_choices() -> Result<()> {
    for multiple in [false, true] {
        let mut app = ordinary_app_with_questions(vec![choice_question(multiple)])?;
        if multiple {
            press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE)?;
        }
        press(&mut app, KeyCode::Char('甲'), KeyModifiers::NONE)?;
        press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE)?;
        press(&mut app, KeyCode::Char('乙'), KeyModifiers::NONE)?;
        let expected = match &app.pending_user_input().expect("form").drafts[0] {
            UserInputDraftValue::SingleSelect {
                selected,
                selected_by_user,
                other,
            } => {
                assert_eq!(*selected, Some(1));
                assert!(*selected_by_user);
                assert_eq!(other, "甲 乙");
                sigil_kernel::UserInputAnswerValueV1::SingleSelect {
                    option_id: None,
                    other: Some(other.clone()),
                }
            }
            UserInputDraftValue::MultiSelect {
                cursor,
                selected,
                other,
            } => {
                assert_eq!(*cursor, 1);
                assert_eq!(selected, &["suggested".to_owned()]);
                assert_eq!(other, "甲 乙");
                sigil_kernel::UserInputAnswerValueV1::MultiSelect {
                    option_ids: selected.clone(),
                    other: Some(other.clone()),
                }
            }
            _ => panic!("choice draft"),
        };
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE)?;
        press(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT)?;
        assert!(
            app.pending_user_input()
                .expect("form")
                .other_answer_focused()
        );
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?;
        assert!(
            matches!(press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?, Some(AppAction::SubmitUserInputDecision {
            decision: sigil_kernel::UserInputDecisionV1::Submitted { answers }, ..
        }) if answers[0].value == expected)
        );
    }
    Ok(())
}

#[test]
fn ordinary_form_empty_other_keeps_text_focus_and_options_keep_space_toggle() -> Result<()> {
    for multiple in [false, true] {
        let mut app = ordinary_app_with_questions(vec![
            choice_question(multiple),
            ordinary_question("next"),
        ])?;
        app.handle_paste_text("甲");
        press(&mut app, KeyCode::Backspace, KeyModifiers::NONE)?;
        assert!(
            app.pending_user_input()
                .expect("form")
                .other_answer_focused()
        );
        press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE)?;
        match &app.pending_user_input().expect("form").drafts[0] {
            UserInputDraftValue::SingleSelect { other, .. }
            | UserInputDraftValue::MultiSelect { other, .. } => assert_eq!(other, " "),
            _ => panic!("choice draft"),
        }
        press(&mut app, KeyCode::Up, KeyModifiers::NONE)?;
        assert!(
            !app.pending_user_input()
                .expect("form")
                .other_answer_focused()
        );
        press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE)?;
        match &app.pending_user_input().expect("form").drafts[0] {
            UserInputDraftValue::SingleSelect {
                selected, other, ..
            } => {
                assert_eq!(*selected, Some(0));
                assert_eq!(other, " ");
            }
            UserInputDraftValue::MultiSelect {
                selected, other, ..
            } => {
                assert_eq!(selected, &["suggested".to_owned()]);
                assert_eq!(other, " ");
            }
            _ => panic!("choice draft"),
        }
        press(&mut app, KeyCode::Tab, KeyModifiers::NONE)?;
        assert_eq!(app.pending_user_input().expect("form").focused_question, 1);
        press(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT)?;
        assert_eq!(app.pending_user_input().expect("form").focused_question, 0);
        assert!(
            !app.pending_user_input()
                .expect("form")
                .other_answer_focused()
        );
    }
    Ok(())
}

#[test]
fn ordinary_form_arrow_selection_reveals_a_hidden_question_after_manual_scroll() -> Result<()> {
    for (key, scroll_key) in [
        (KeyCode::Left, KeyCode::Home),
        (KeyCode::Right, KeyCode::PageUp),
    ] {
        let mut app = host_question_group_app()?;
        app.set_terminal_size(80, 12);
        app.composer
            .pending_user_input
            .as_mut()
            .expect("form")
            .view
            .questions[0]
            .description = Some("First question context. ".repeat(40));
        let _ = ordinary_surface_text(&app, 12)?;
        press(&mut app, KeyCode::Tab, KeyModifiers::NONE)?;
        let rendered = ordinary_surface_text(&app, 12)?;
        assert!(rendered.contains("检查哪些界面？"), "{key:?}: {rendered}");
        press(&mut app, scroll_key, KeyModifiers::NONE)?;
        let rendered = ordinary_surface_text(&app, 12)?;
        assert!(
            !rendered.contains("检查哪些界面？"),
            "{scroll_key:?}: {rendered}"
        );
        press(&mut app, key, KeyModifiers::NONE)?;
        let rendered = ordinary_surface_text(&app, 12)?;
        assert!(rendered.contains("检查哪些界面？"), "{key:?}: {rendered}");
        assert!(rendered.contains("[x] TUI"), "{key:?}: {rendered}");
        let form = app.pending_user_input().expect("form");
        assert_eq!(form.focused_question, 1);
        assert!(
            matches!(&form.drafts[1], UserInputDraftValue::MultiSelect { selected, .. }
            if selected == &["suggested".to_owned()])
        );
    }
    Ok(())
}

#[test]
fn ordinary_form_other_focus_reveals_its_wrapped_row_and_keeps_manual_scroll() -> Result<()> {
    for multiple in [false, true] {
        let mut question = choice_question(multiple);
        question.options[0].label = "A long option label. ".repeat(40);
        let mut app = ordinary_app_with_questions(vec![question])?;
        app.set_terminal_size(80, 12);
        let _ = ordinary_surface_text(&app, 12)?;
        press(&mut app, KeyCode::Char('甲'), KeyModifiers::NONE)?;
        press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE)?;
        press(&mut app, KeyCode::Char('乙'), KeyModifiers::NONE)?;
        let rendered = ordinary_surface_text(&app, 12)?;
        assert!(
            rendered.contains("Other: 甲 乙"),
            "multiple={multiple}: {rendered}"
        );
        assert!(rendered.contains("Space inserts space"));
        press(&mut app, KeyCode::Home, KeyModifiers::NONE)?;
        let rendered = ordinary_surface_text(&app, 12)?;
        assert!(!rendered.contains("Other: 甲 乙"));
        press(&mut app, KeyCode::Char('丙'), KeyModifiers::NONE)?;
        assert!(ordinary_surface_text(&app, 12)?.contains("Other: 甲 乙丙"));
    }
    Ok(())
}

#[test]
fn ordinary_form_paste_preserves_unicode_multiline_answer_and_original_composer_draft() -> Result<()>
{
    let mut app = ordinary_app_with_questions(vec![ordinary_question("note")])?;
    let request = app
        .pending_user_input()
        .expect("form")
        .request
        .clone()
        .expect("request");
    app.composer.input = "原来的草稿 👨‍👩‍👧‍👦\n不能改变".to_owned();
    app.composer.input_cursor = 3;
    app.handle_paste_text("  中文 👩🏽‍💻\r\n第二行\n");
    assert_eq!(text(&app), "  中文 👩🏽‍💻\n第二行\n");
    assert_eq!(app.composer.input, "原来的草稿 👨‍👩‍👧‍👦\n不能改变");
    assert_eq!(app.composer.input_cursor, 3);
    assert!(app.composer.input_paste_spans.is_empty());
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?;
    let action = press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?;
    assert!(matches!(
        action,
        Some(AppAction::SubmitUserInputDecision {
            request_id,
            generation,
            expected_request_hash,
            decision: sigil_kernel::UserInputDecisionV1::Submitted { answers },
            ..
        }) if request_id == request.identity.request_id.as_str()
            && generation == request.identity.generation
            && expected_request_hash == request.request_hash
            && answers == vec![sigil_kernel::UserInputAnswerV1 {
                question_id: "note".to_owned(),
                value: sigil_kernel::UserInputAnswerValueV1::Text {
                    value: "  中文 👩🏽‍💻\n第二行\n".to_owned(),
                },
            }]
    ));
    Ok(())
}

#[test]
fn ordinary_form_choice_paste_targets_other_and_preserves_multi_select_choices() -> Result<()> {
    for multiple in [false, true] {
        let mut app = ordinary_app_with_questions(vec![choice_question(multiple)])?;
        press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE)?;
        app.handle_paste_text("自己的选择 ✅\n保留详情");
        assert!(app.composer.input.is_empty());
        let form = app.pending_user_input().expect("form");
        match &form.drafts[0] {
            UserInputDraftValue::SingleSelect {
                selected,
                selected_by_user,
                other,
            } => {
                assert_eq!(*selected, Some(1));
                assert!(*selected_by_user);
                assert_eq!(other, "自己的选择 ✅\n保留详情");
            }
            UserInputDraftValue::MultiSelect {
                selected, other, ..
            } => {
                assert_eq!(selected, &["suggested".to_owned()]);
                assert_eq!(other, "自己的选择 ✅\n保留详情");
            }
            UserInputDraftValue::Text(_) => panic!("expected choice draft"),
        }
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?;
        let action = press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?;
        let Some(AppAction::SubmitUserInputDecision {
            decision: sigil_kernel::UserInputDecisionV1::Submitted { answers },
            ..
        }) = action
        else {
            panic!("expected submitted choice");
        };
        let expected = if multiple {
            sigil_kernel::UserInputAnswerValueV1::MultiSelect {
                option_ids: vec!["suggested".to_owned()],
                other: Some("自己的选择 ✅\n保留详情".to_owned()),
            }
        } else {
            sigil_kernel::UserInputAnswerValueV1::SingleSelect {
                option_id: None,
                other: Some("自己的选择 ✅\n保留详情".to_owned()),
            }
        };
        assert_eq!(answers[0].value, expected);
    }
    Ok(())
}

#[test]
fn ordinary_form_space_confirms_the_highlighted_option_or_other_before_changing_it() -> Result<()> {
    let mut question = choice_question(false);
    question.options.push(sigil_kernel::UserInputOptionV1 {
        id: "second".to_owned(),
        label: "第二个选项".to_owned(),
        description: None,
    });
    let mut app = ordinary_app_with_questions(vec![question.clone()])?;
    press(&mut app, KeyCode::Down, KeyModifiers::NONE)?;
    press(&mut app, KeyCode::Down, KeyModifiers::NONE)?;
    press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE)?;
    assert!(matches!(
        &app.pending_user_input().expect("form").drafts[0],
        UserInputDraftValue::SingleSelect {
            selected: Some(1),
            selected_by_user: true,
            ..
        }
    ));
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?;
    assert!(
        matches!(press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?,
            Some(AppAction::SubmitUserInputDecision { decision: sigil_kernel::UserInputDecisionV1::Submitted { answers }, .. })
                if matches!(&answers[0].value, sigil_kernel::UserInputAnswerValueV1::SingleSelect { option_id: Some(id), other: None } if id == "second")
        )
    );

    let mut app = ordinary_app_with_questions(vec![question])?;
    press(&mut app, KeyCode::Up, KeyModifiers::NONE)?;
    press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE)?;
    assert!(matches!(&app.pending_user_input().expect("form").drafts[0],
        UserInputDraftValue::SingleSelect { selected: Some(2), selected_by_user: true, other } if other.is_empty()));
    app.handle_paste_text("其他回答");
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?;
    assert!(
        matches!(press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?,
            Some(AppAction::SubmitUserInputDecision { decision: sigil_kernel::UserInputDecisionV1::Submitted { answers }, .. })
                if matches!(&answers[0].value, sigil_kernel::UserInputAnswerValueV1::SingleSelect { option_id: None, other: Some(value) } if value == "其他回答")
        )
    );
    Ok(())
}

#[test]
fn ordinary_form_action_paste_is_consumed_with_visible_feedback() -> Result<()> {
    let mut app = ordinary_app_with_questions(vec![ordinary_question("note")])?;
    app.composer.input = "keep composer".to_owned();
    app.handle_paste_text("keep answer");
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE)?;
    app.handle_paste_text("must not reach another input");
    let form = app.pending_user_input().expect("form");
    assert!(form.focus_actions);
    assert_eq!(text(&app), "keep answer");
    assert_eq!(app.composer.input, "keep composer");
    assert_eq!(
        form.validation_error
            .as_ref()
            .expect("feedback")
            .question_index,
        None
    );
    assert!(
        form.validation_error
            .as_ref()
            .expect("feedback")
            .message
            .contains("↑")
    );
    press(&mut app, KeyCode::Up, KeyModifiers::NONE)?;
    assert!(!app.pending_user_input().expect("form").focus_actions);
    assert!(
        app.pending_user_input()
            .expect("form")
            .validation_error
            .is_none()
    );
    Ok(())
}

#[test]
fn ordinary_form_recovery_paste_cannot_change_an_accepted_answer_or_composer() -> Result<()> {
    let mut app = ordinary_app_with_questions(vec![ordinary_question("note")])?;
    let request = app
        .pending_user_input()
        .expect("form")
        .request
        .clone()
        .expect("request");
    let command = sigil_kernel::UserInputDecisionCommandV1 {
        identity: request.identity.clone(),
        request_hash: request.request_hash.clone(),
        command_id: sigil_kernel::UserInputCommandId::new("form-paste-recovery")?,
        decision: sigil_kernel::UserInputDecisionV1::Submitted {
            answers: vec![sigil_kernel::UserInputAnswerV1 {
                question_id: "note".to_owned(),
                value: sigil_kernel::UserInputAnswerValueV1::Text {
                    value: "original accepted answer".to_owned(),
                },
            }],
        },
    };
    app.clear_pending_user_input();
    app.set_pending_user_input_recovery(request.clone(), command.clone());
    app.composer.input = "original composer".to_owned();
    app.handle_paste_text("cannot replace accepted answer");
    let form = app.pending_user_input().expect("form");
    assert_eq!(form.recovery_command.as_ref(), Some(&command));
    assert_eq!(app.composer.input, "original composer");
    assert_eq!(text(&app), "");
    assert!(
        form.validation_error
            .as_ref()
            .expect("paste feedback")
            .message
            .contains("cannot be edited")
    );
    assert!(matches!(
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?,
        Some(AppAction::ResumeCommittedUserInput { original_command_id, request_id, .. })
            if original_command_id == command.command_id.as_str()
                && request_id == request.identity.request_id.as_str()
    ));
    Ok(())
}

#[test]
fn ordinary_form_closed_paste_edits_composer_and_reopen_retains_answer() -> Result<()> {
    let mut app = ordinary_app_with_questions(vec![ordinary_question("note")])?;
    app.handle_paste_text("保留回答 🔖");
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE)?;
    app.handle_paste_text("现在输入主草稿\n第二行");
    assert_eq!(app.composer.input, "现在输入主草稿\n第二行");
    assert_eq!(text(&app), "保留回答 🔖");
    press(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT)?;
    assert!(app.pending_user_input().expect("form").open);
    app.handle_paste_text(" + 回到回答");
    assert_eq!(text(&app), "保留回答 🔖 + 回到回答");
    assert_eq!(app.composer.input, "现在输入主草稿\n第二行");
    Ok(())
}

#[test]
fn ordinary_form_validation_focuses_first_missing_answer_and_clears_only_when_corrected()
-> Result<()> {
    let mut app = ordinary_app_with_questions(vec![
        ordinary_question("first"),
        ordinary_question("second"),
    ])?;
    let form = app.composer.pending_user_input.as_mut().expect("form");
    *form.question_scroll_offsets.borrow_mut() = vec![2, 31];
    form.scroll_extent.set(40);
    press(&mut app, KeyCode::Enter, KeyModifiers::CONTROL)?;
    assert!(press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?.is_none());
    let form = app.pending_user_input().expect("form");
    assert_eq!(form.focused_question, 0);
    assert!(!form.focus_actions);
    assert_eq!(form.scroll, 2);
    assert_eq!(
        form.validation_error
            .as_ref()
            .expect("error")
            .question_index,
        Some(0)
    );
    press(&mut app, KeyCode::Backspace, KeyModifiers::NONE)?;
    assert!(
        app.pending_user_input()
            .expect("form")
            .validation_error
            .is_some()
    );
    app.handle_paste_text("first answer");
    assert!(
        app.pending_user_input()
            .expect("form")
            .validation_error
            .is_none()
    );
    press(&mut app, KeyCode::Enter, KeyModifiers::CONTROL)?;
    assert!(press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?.is_none());
    let form = app.pending_user_input().expect("form");
    assert_eq!(form.focused_question, 1);
    assert!(!form.focus_actions);
    assert_eq!(form.scroll, 31);
    assert_eq!(
        form.validation_error
            .as_ref()
            .expect("error")
            .question_index,
        Some(1)
    );
    assert!(
        form.validation_error
            .as_ref()
            .expect("error")
            .message
            .contains("question 2")
    );
    app.handle_paste_text("second answer");
    assert!(
        app.pending_user_input()
            .expect("form")
            .validation_error
            .is_none()
    );
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?;
    assert!(press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?.is_some());
    Ok(())
}

#[test]
fn ordinary_form_empty_other_is_invalid_until_text_is_pasted() -> Result<()> {
    let mut app = ordinary_app_with_questions(vec![choice_question(false)])?;
    let form = app.composer.pending_user_input.as_mut().expect("form");
    form.drafts[0] = UserInputDraftValue::SingleSelect {
        selected: Some(1),
        selected_by_user: true,
        other: String::new(),
    };
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?;
    assert!(press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?.is_none());
    assert_eq!(app.pending_user_input().expect("form").focused_question, 0);
    assert!(
        app.pending_user_input()
            .expect("form")
            .validation_error
            .is_some()
    );
    app.handle_paste_text("真实回答");
    assert!(
        app.pending_user_input()
            .expect("form")
            .validation_error
            .is_none()
    );
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?;
    assert!(press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?.is_some());
    Ok(())
}

#[test]
fn ordinary_form_actions_return_to_the_same_rendered_question_without_submitting() -> Result<()> {
    let mut app = ordinary_app_with_questions(vec![
        ordinary_question("first"),
        ordinary_question("second"),
    ])?;
    let form = app.composer.pending_user_input.as_mut().expect("form");
    *form.question_scroll_offsets.borrow_mut() = vec![2, 31];
    form.scroll_extent.set(40);
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE)?;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE)?;
    assert!(app.pending_user_input().expect("form").focus_actions);
    assert!(press(&mut app, KeyCode::Right, KeyModifiers::NONE)?.is_none());
    assert!(press(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT)?.is_none());
    assert_eq!(
        app.pending_user_input().expect("form").selected_action,
        UserInputFormAction::Submit
    );
    assert!(press(&mut app, KeyCode::Up, KeyModifiers::NONE)?.is_none());
    let form = app.pending_user_input().expect("form");
    assert!(!form.focus_actions);
    assert_eq!(form.focused_question, 1);
    assert_eq!(form.scroll, 31);
    press(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT)?;
    assert_eq!(app.pending_user_input().expect("form").focused_question, 0);
    assert_eq!(app.pending_user_input().expect("form").scroll, 2);
    Ok(())
}

fn routed_attention_fixture() -> Result<(
    sigil_kernel::AgentUserInputRouteEntryV1,
    sigil_kernel::UserInputDecisionCommandV1,
)> {
    let public = crate::app::tests::worker_bridge_tests::pending_text_user_input_request()?;
    let task_id = sigil_kernel::TaskId::new("attention-task")?;
    let requested = sigil_kernel::UserInputRequestedV1::new(sigil_kernel::UserInputRequestV1 {
        schema_version: sigil_kernel::USER_INPUT_SCHEMA_VERSION,
        identity: public.identity,
        source: sigil_kernel::UserInputSourceV1::Agent,
        purpose: public.purpose,
        prompt: public.prompt,
        prompt_origin: None,
        questions: public.questions,
        allowed_actions: public.allowed_actions,
        requested_at_unix_ms: 1,
        continuation: Some(sigil_kernel::UserInputContinuationBindingV1 {
            assistant_message_id: "attention-assistant".to_owned(),
            tool_call_id: "attention-call".to_owned(),
            provider_name: "fixture".to_owned(),
            model_name: "model".to_owned(),
        }),
    })?;
    let command = sigil_kernel::UserInputDecisionCommandV1 {
        identity: requested.request.identity.clone(),
        request_hash: requested.request_hash.clone(),
        command_id: sigil_kernel::UserInputCommandId::new("attention-command")?,
        decision: sigil_kernel::UserInputDecisionV1::Submitted {
            answers: vec![sigil_kernel::UserInputAnswerV1 {
                question_id: "scope".to_owned(),
                value: sigil_kernel::UserInputAnswerValueV1::Text {
                    value: "private saved answer".to_owned(),
                },
            }],
        },
    };
    let route = sigil_kernel::AgentUserInputRouteEntryV1 {
        schema_version: sigil_kernel::AGENT_USER_INPUT_ROUTE_SCHEMA_VERSION,
        route_id: sigil_kernel::AgentRouteId::new("attention-route")?,
        source_thread_id: command.identity.source_thread_id.clone(),
        source_attempt_id: sigil_kernel::AgentRunAttemptId::new("attention-attempt")?,
        profile_id: sigil_kernel::AgentProfileId::new("agent")?,
        parent_thread_id: sigil_kernel::AgentThreadId::new("root")?,
        batch_id: None,
        budget_scope_id: task_id,
        isolation: sigil_kernel::TaskIsolationMode::SharedReadOnly,
        child_session_ref: sigil_kernel::SessionRef::new_relative("children/attention.jsonl")?,
        request: sigil_kernel::UserInputRequestStateV1 {
            requested,
            status: sigil_kernel::UserInputStatusV1::Requested,
            decision: None,
            claim: None,
            continuation: None,
            resolution: None,
        }
        .public_view(),
        status: sigil_kernel::AgentRouteStatus::Requested,
        updated_at_unix_ms: 1,
    };
    Ok((route, command))
}

fn attention_route_entry(
    route: &sigil_kernel::AgentUserInputRouteEntryV1,
) -> sigil_kernel::SessionLogEntry {
    sigil_kernel::SessionLogEntry::Control(sigil_kernel::ControlEntry::AgentUserInputRoute(
        route.clone(),
    ))
}

#[test]
fn applied_child_answer_stays_dismissed_through_stale_refresh_registered_and_resolved() -> Result<()>
{
    let (mut route, command) = routed_attention_fixture()?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let stale = vec![attention_route_entry(&route)];
    app.session_browser.current_entries = stale.clone();
    app.restore_durable_attention_surfaces();
    assert!(app.pending_user_input().is_some());
    app.handle_worker_message(crate::runner::WorkerMessage::UserInputDecisionApplied {
        request: route.request.clone(),
        continuation_started: true,
        entries: stale.clone(),
    })?;
    app.restore_durable_attention_surfaces();
    // A same-revision auxiliary result may still carry the private accepted command.
    app.restore_durable_attention_surfaces_with_recovery_command(command.clone());
    assert!(app.runtime.is_busy);
    assert!(app.pending_user_input().is_none());
    assert!(app.composer.pending_user_input_queue.is_empty());
    route.status = sigil_kernel::AgentRouteStatus::Registered;
    route.updated_at_unix_ms = 2;
    app.append_current_session_control(sigil_kernel::ControlEntry::AgentUserInputRoute(
        route.clone(),
    ));
    assert!(app.user_input_attention_is_submitted(&command.identity, &command.request_hash));
    app.restore_durable_attention_surfaces();
    assert!(app.pending_user_input().is_none());
    route.status = sigil_kernel::AgentRouteStatus::Resolved;
    route.updated_at_unix_ms = 3;
    app.append_current_session_control(sigil_kernel::ControlEntry::AgentUserInputRoute(
        route.clone(),
    ));
    assert!(app.session_auxiliary.submitted_user_inputs.is_empty());
    let current = app.session_browser.current_entries.clone();
    for message in [
        crate::runner::WorkerMessage::UserInputRequested {
            request: route.request.clone(),
            entries: stale.clone(),
        },
        crate::runner::WorkerMessage::RecoveredUserInputAttention {
            command,
            entries: stale.clone(),
        },
        crate::runner::WorkerMessage::UserInputDecisionApplied {
            request: route.request,
            continuation_started: false,
            entries: stale,
        },
    ] {
        app.handle_worker_message(message)?;
        assert!(app.pending_user_input().is_none());
        assert!(app.composer.pending_user_input_queue.is_empty());
        assert_eq!(
            serde_json::to_value(&app.session_browser.current_entries)?,
            serde_json::to_value(&current)?
        );
        assert!(
            app.runtime.is_busy,
            "late old input messages cannot change the current run"
        );
    }
    Ok(())
}

#[test]
fn stopped_worker_can_recover_requested_and_registered_answers_but_busy_worker_cannot() -> Result<()>
{
    for registered in [false, true] {
        let (mut route, command) = routed_attention_fixture()?;
        let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
        let stale = vec![attention_route_entry(&route)];
        app.session_browser.current_entries = stale.clone();
        app.dismiss_submitted_user_input(&route.request);
        if registered {
            route.status = sigil_kernel::AgentRouteStatus::Registered;
            route.updated_at_unix_ms = 2;
            app.append_current_session_control(sigil_kernel::ControlEntry::AgentUserInputRoute(
                route.clone(),
            ));
        }
        let current = app.session_browser.current_entries.clone();
        app.runtime.is_busy = true;
        app.handle_worker_message(crate::runner::WorkerMessage::RecoveredUserInputAttention {
            command: command.clone(),
            entries: stale.clone(),
        })?;
        assert!(app.pending_user_input().is_none());
        assert_eq!(
            serde_json::to_value(&app.session_browser.current_entries)?,
            serde_json::to_value(&current)?
        );
        app.runtime.is_busy = false;
        app.handle_worker_message(crate::runner::WorkerMessage::RecoveredUserInputAttention {
            command: command.clone(),
            entries: stale,
        })?;
        for _ in 0..2 {
            let form = app.pending_user_input().expect("explicit recovered form");
            assert_eq!(form.recovery_command.as_ref(), Some(&command));
            assert_eq!(form.selected_action, UserInputFormAction::Resume);
            app.restore_durable_attention_surfaces();
        }
        assert_eq!(
            serde_json::to_value(&app.session_browser.current_entries)?,
            serde_json::to_value(&current)?
        );
    }
    Ok(())
}

#[test]
fn registered_control_dismisses_only_its_input_and_preserves_other_draft_and_mcp() -> Result<()> {
    let (mut route, _) = routed_attention_fixture()?;
    let (mut other, _) = routed_attention_fixture()?;
    other.route_id = sigil_kernel::AgentRouteId::new("other-attention-route")?;
    other.request.identity.generation += 1;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.session_browser.current_entries =
        vec![attention_route_entry(&route), attention_route_entry(&other)];
    app.runtime.is_busy = true;
    app.restore_durable_attention_surfaces();
    assert_eq!(
        app.composer.pending_user_input_queue.len(),
        2,
        "busy does not hide a genuine question"
    );
    let draft = app
        .composer
        .pending_user_input_queue
        .get_mut(1)
        .expect("second request");
    draft.drafts[0] = UserInputDraftValue::Text("keep this unsent draft".to_owned());
    draft.open = false;
    route.status = sigil_kernel::AgentRouteStatus::Registered;
    route.updated_at_unix_ms = 2;
    app.append_current_session_control(sigil_kernel::ControlEntry::AgentUserInputRoute(route));
    let remaining = app.pending_user_input().expect("other request");
    assert_eq!(
        remaining
            .request
            .as_ref()
            .expect("remaining input request")
            .identity
            .generation,
        2
    );
    assert!(
        matches!(&remaining.drafts[0], UserInputDraftValue::Text(value) if value == "keep this unsent draft")
    );
    assert!(!remaining.open);
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    app.set_pending_mcp_user_input(
        remaining.view.clone(),
        remaining.drafts.clone(),
        "fixture".to_owned(),
        tx,
    );
    app.restore_durable_attention_surfaces();
    assert!(matches!(
        app.pending_user_input()
            .expect("MCP form remains open")
            .source,
        UserInputFormSource::Mcp { .. }
    ));
    assert!(matches!(
        rx.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    assert!(app.pending_mcp_elicitation.is_some());
    Ok(())
}

#[test]
fn input_preparation_failure_reconciles_terminal_entries_and_preserves_other_attention()
-> Result<()> {
    let (mut route, command) = routed_attention_fixture()?;
    let (mut other, _) = routed_attention_fixture()?;
    other.route_id = sigil_kernel::AgentRouteId::new("other-attention-route")?;
    other.request.identity.generation += 1;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.session_browser.current_entries =
        vec![attention_route_entry(&route), attention_route_entry(&other)];
    app.restore_durable_attention_surfaces();
    app.set_pending_user_input_recovery(route.request.clone(), command.clone());
    let draft = app
        .composer
        .pending_user_input_queue
        .get_mut(1)
        .expect("second input");
    draft.drafts[0] = UserInputDraftValue::Text("preserve failure-side draft".to_owned());
    draft.open = false;
    route.status = sigil_kernel::AgentRouteStatus::Resolved;
    route.updated_at_unix_ms = 2;
    let mut terminal_entries = app.session_browser.current_entries.clone();
    terminal_entries.push(attention_route_entry(&route));
    app.handle_worker_message(crate::runner::WorkerMessage::UserInputDecisionFailed {
        request_id: command.identity.request_id.as_str().to_owned(),
        generation: command.identity.generation,
        expected_request_hash: command.request_hash.clone(),
        message: "provider consumption is uncertain".to_owned(),
        entries: Some(terminal_entries.clone()),
    })?;
    assert_eq!(
        serde_json::to_value(&app.session_browser.current_entries)?,
        serde_json::to_value(&terminal_entries)?
    );
    assert_eq!(
        app.last_notice.as_deref(),
        Some("provider consumption is uncertain")
    );
    let remaining = app
        .pending_user_input()
        .expect("unrelated question survives");
    assert_eq!(
        remaining
            .request
            .as_ref()
            .expect("remaining input request")
            .identity,
        other.request.identity
    );
    assert!(
        matches!(&remaining.drafts[0], UserInputDraftValue::Text(value) if value == "preserve failure-side draft")
    );
    assert!(!remaining.open);
    assert_eq!(app.composer.pending_user_input_queue.len(), 1);
    app.restore_durable_attention_surfaces();
    assert!(
        app.composer
            .pending_user_input_queue
            .iter()
            .all(|form| form.recovery_command.is_none())
    );
    Ok(())
}

#[test]
fn ordinary_resume_failure_clears_a_durably_resolved_form_without_marking_answer_applied()
-> Result<()> {
    let mut session = sigil_kernel::Session::new("fixture", "model");
    let mut public = crate::app::tests::worker_bridge_tests::pending_text_user_input_request()?;
    public.identity.session_scope_id =
        sigil_kernel::SessionScopeId::new(session.session_scope_id())?;
    let requested = sigil_kernel::UserInputRequestedV1::new(sigil_kernel::UserInputRequestV1 {
        schema_version: sigil_kernel::USER_INPUT_SCHEMA_VERSION,
        identity: public.identity,
        source: public.source,
        purpose: public.purpose,
        prompt: public.prompt,
        prompt_origin: None,
        questions: public.questions,
        allowed_actions: public.allowed_actions,
        requested_at_unix_ms: 1,
        continuation: Some(sigil_kernel::UserInputContinuationBindingV1 {
            assistant_message_id: "attention-assistant".to_owned(),
            tool_call_id: "attention-call".to_owned(),
            provider_name: "fixture".to_owned(),
            model_name: "model".to_owned(),
        }),
    })?;
    session.append_user_input_lifecycle(vec![
        sigil_kernel::UserInputLifecycleEntryV1::Requested(Box::new(requested.clone())),
    ])?;
    let command = sigil_kernel::UserInputDecisionCommandV1 {
        identity: requested.request.identity.clone(),
        request_hash: requested.request_hash.clone(),
        command_id: sigil_kernel::UserInputCommandId::new("failed-resume-answer")?,
        decision: sigil_kernel::UserInputDecisionV1::Submitted {
            answers: vec![sigil_kernel::UserInputAnswerV1 {
                question_id: "scope".to_owned(),
                value: sigil_kernel::UserInputAnswerValueV1::Text {
                    value: "private answer".to_owned(),
                },
            }],
        },
    };
    sigil_kernel::accept_user_input_decision(&mut session, command.clone(), 2)?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.session_browser.current_entries = session.entries().to_vec();
    app.restore_durable_attention_surfaces();
    assert_eq!(
        app.pending_user_input().expect("resume").selected_action,
        UserInputFormAction::Resume
    );
    let claim_id = sigil_kernel::UserInputClaimId::new("failed-resume-claim")?;
    session.append_user_input_lifecycle(vec![
        sigil_kernel::UserInputLifecycleEntryV1::ContinuationClaimed(
            sigil_kernel::UserInputContinuationClaimedV1 {
                schema_version: sigil_kernel::USER_INPUT_SCHEMA_VERSION,
                identity: command.identity.clone(),
                request_hash: command.request_hash.clone(),
                claim_id: claim_id.clone(),
                supervisor_instance_id: "failed-resume-supervisor".to_owned(),
                claimed_at_unix_ms: 3,
            },
        ),
        sigil_kernel::UserInputLifecycleEntryV1::ContinuationStarted(
            sigil_kernel::UserInputContinuationStartedV1 {
                schema_version: sigil_kernel::USER_INPUT_SCHEMA_VERSION,
                identity: command.identity.clone(),
                request_hash: command.request_hash.clone(),
                claim_id,
                continuation_logical_run_id: sigil_kernel::LogicalRunId::new(
                    "failed-resume-continuation",
                )?,
                physical_attempt_id: "failed-resume-attempt".to_owned(),
                started_at_unix_ms: 4,
            },
        ),
        sigil_kernel::UserInputLifecycleEntryV1::Resolved(sigil_kernel::UserInputResolvedV1 {
            schema_version: sigil_kernel::USER_INPUT_SCHEMA_VERSION,
            identity: command.identity.clone(),
            request_hash: command.request_hash.clone(),
            resolution: sigil_kernel::UserInputResolutionV1::Failed {
                failure_class: "provider_attempt_outcome_uncertain".to_owned(),
                retryable: false,
            },
            resolved_at_unix_ms: 5,
        }),
    ])?;
    app.handle_worker_message(crate::runner::WorkerMessage::UserInputDecisionFailed {
        request_id: command.identity.request_id.as_str().to_owned(),
        generation: command.identity.generation,
        expected_request_hash: command.request_hash,
        message: "continuation cannot be replayed".to_owned(),
        entries: Some(session.entries().to_vec()),
    })?;
    app.restore_durable_attention_surfaces();
    assert!(app.pending_user_input().is_none());
    assert!(app.composer.pending_user_input_queue.is_empty());
    assert!(!app.runtime.is_busy);
    assert!(
        app.events
            .iter()
            .all(|event| event.label != "user_input:decision")
    );
    Ok(())
}
