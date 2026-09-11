use super::*;

fn press(editor: &mut PlanRevisionEditorState, value: &mut String, code: KeyCode) {
    editor.handle_key(value, KeyEvent::new(code, KeyModifiers::NONE));
}

#[test]
fn plan_revision_editor_inserts_and_deletes_at_grapheme_boundaries() {
    let mut editor = PlanRevisionEditorState::default();
    let mut value = String::new();
    editor.insert(&mut value, "你👩‍💻e\u{301}");

    press(&mut editor, &mut value, KeyCode::Left);
    assert_eq!(editor.cursor, "你👩‍💻".len());
    press(&mut editor, &mut value, KeyCode::Backspace);
    assert_eq!(value, "你e\u{301}");
    assert_eq!(editor.cursor, "你".len());
    editor.insert(&mut value, "middle");
    assert_eq!(value, "你middlee\u{301}");
    press(&mut editor, &mut value, KeyCode::Delete);
    assert_eq!(value, "你middle");
    press(&mut editor, &mut value, KeyCode::Home);
    press(&mut editor, &mut value, KeyCode::Delete);
    assert_eq!(value, "middle");
    assert_eq!(editor.cursor, 0);
}

#[test]
fn plan_revision_editor_home_end_stay_on_the_current_line() {
    let mut editor = PlanRevisionEditorState::default();
    let mut value = String::new();
    editor.insert(&mut value, "first\nsecond\nthird");
    press(&mut editor, &mut value, KeyCode::Up);
    press(&mut editor, &mut value, KeyCode::Home);
    assert_eq!(editor.cursor, "first\n".len());
    editor.insert(&mut value, "new ");
    press(&mut editor, &mut value, KeyCode::End);
    assert_eq!(editor.cursor, "first\nnew second".len());
    press(&mut editor, &mut value, KeyCode::Delete);
    assert_eq!(value, "first\nnew secondthird");
    editor.handle_key(
        &mut value,
        KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL),
    );
    assert_eq!(editor.cursor, 0);
    editor.handle_key(
        &mut value,
        KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL),
    );
    assert_eq!(editor.cursor, value.len());
}

#[test]
fn plan_revision_editor_vertical_motion_retains_the_desired_display_column() {
    let mut editor = PlanRevisionEditorState::default();
    let mut value = "ab你d\nx\nab你d".to_owned();
    editor.viewport.set((20, 4));
    editor.cursor = "ab你".len();

    press(&mut editor, &mut value, KeyCode::Down);
    assert_eq!(editor.cursor, "ab你d\nx".len());
    press(&mut editor, &mut value, KeyCode::Down);
    assert_eq!(editor.cursor, "ab你d\nx\nab你".len());
    press(&mut editor, &mut value, KeyCode::Up);
    press(&mut editor, &mut value, KeyCode::Up);
    assert_eq!(editor.cursor, "ab你".len());
}

#[test]
fn plan_revision_editor_wrap_navigation_and_explicit_newline_share_the_rendered_rows() {
    let mut editor = PlanRevisionEditorState::default();
    let mut value = "abcdef".to_owned();
    editor.viewport.set((3, 3));
    assert_eq!(editor.layout(&value, 3).rows, ["abc", "def", ""]);
    press(&mut editor, &mut value, KeyCode::Down);
    assert_eq!(editor.cursor, 3);
    press(&mut editor, &mut value, KeyCode::Down);
    assert_eq!(editor.cursor, 6);

    value = "abc\nx".to_owned();
    editor.cursor = 0;
    press(&mut editor, &mut value, KeyCode::Down);
    assert_eq!(editor.cursor, "abc\n".len());
    editor.insert(&mut value, "new ");
    assert_eq!(value, "abc\nnew x");
}

#[test]
fn plan_revision_editor_submission_freezes_the_exact_draft() {
    let mut editor = PlanRevisionEditorState::default();
    let mut value = String::new();
    editor.insert(&mut value, "submitted");
    editor.submitting = true;

    editor.insert(&mut value, "paste\nmore");
    press(&mut editor, &mut value, KeyCode::Home);
    press(&mut editor, &mut value, KeyCode::Backspace);
    press(&mut editor, &mut value, KeyCode::Char('x'));
    assert_eq!(value, "submitted");
    assert_eq!(editor.cursor, value.len());
}
