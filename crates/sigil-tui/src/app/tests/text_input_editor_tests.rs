use super::*;

fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, modifiers)
}

#[test]
fn novice_modal_edit_inserts_replaces_and_undoes_at_the_real_cursor() {
    let mut text = "existing-model".to_owned();
    let mut editor = TextInputEditor::new(&text);
    editor.handle_key(&mut text, key(KeyCode::Home, KeyModifiers::NONE));
    editor.insert(&mut text, "X");
    assert_eq!(text, "Xexisting-model");
    editor.handle_key(&mut text, key(KeyCode::Right, KeyModifiers::SHIFT));
    editor.handle_key(&mut text, key(KeyCode::Right, KeyModifiers::SHIFT));
    editor.insert(&mut text, "中文");
    assert_eq!(text, "X中文isting-model");
    editor.handle_key(&mut text, key(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(text, "Xexisting-model");
    editor.handle_key(&mut text, key(KeyCode::Char('y'), KeyModifiers::CONTROL));
    assert_eq!(text, "X中文isting-model");
    editor.handle_key(&mut text, key(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert!(text.is_empty());
    editor.handle_key(&mut text, key(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(text, "X中文isting-model");
}

#[test]
fn novice_modal_edit_deletes_whole_graphemes_and_selects_without_splitting_unicode() {
    let mut text = "中e\u{301}👨‍👩‍👧‍👦".to_owned();
    let mut editor = TextInputEditor::new(&text);
    editor.handle_key(&mut text, key(KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(text, "中e\u{301}");
    editor.handle_key(&mut text, key(KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(editor.cursor.position, 1);
    editor.handle_key(&mut text, key(KeyCode::Delete, KeyModifiers::NONE));
    assert_eq!(text, "中");
    editor.handle_key(&mut text, key(KeyCode::Home, KeyModifiers::NONE));
    editor.handle_key(&mut text, key(KeyCode::End, KeyModifiers::SHIFT));
    editor.insert(&mut text, "你好");
    assert_eq!(text, "你好");
    assert_eq!(editor.cursor.position, 2);
}

#[test]
fn novice_modal_edit_history_is_bounded_and_new_edits_retire_redo() {
    let mut text = String::new();
    let mut editor = TextInputEditor::new(&text);
    for _ in 0..110 {
        editor.insert(&mut text, "x");
    }
    assert_eq!(editor.undo.len(), 100);
    editor.handle_key(&mut text, key(KeyCode::Char('z'), KeyModifiers::CONTROL));
    editor.insert(&mut text, "y");
    assert!(editor.redo.is_empty());
}
