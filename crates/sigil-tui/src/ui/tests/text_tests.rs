use super::*;

#[test]
fn wrapped_line_rows_counts_visual_rows() {
    assert_eq!(wrapped_line_rows("", 10), 1);
    assert_eq!(wrapped_line_rows("short", 10), 1);
    assert_eq!(wrapped_line_rows("1234567890", 10), 1);
    assert_eq!(wrapped_line_rows("12345678901", 10), 2);
    assert_eq!(wrapped_line_rows("你好", 2), 2);
}

#[test]
fn truncate_inline_text_handles_short_and_long_inputs() {
    assert_eq!(truncate_inline_text("abc", 5), "abc");
    assert_eq!(truncate_inline_text("abcdef", 3), "abc...");
}

#[test]
fn truncate_display_width_respects_visual_budget_and_ellipsis_width() {
    assert_eq!(truncate_display_width("abc", 10), "abc");
    assert_eq!(truncate_display_width("你好", 2), "你...");
    assert_eq!(truncate_display_width("你好", 5), "你好");
    assert_eq!(truncate_display_width("abc", 1), "a...");
}

#[test]
fn wrap_display_width_preserves_empty_and_multichar_lines() {
    assert_eq!(wrap_display_width("", 10), vec![String::from("")]);
    assert_eq!(
        wrap_display_width("abcdef", 3),
        vec!["abc".to_owned(), "def".to_owned()]
    );
}

#[test]
fn wrap_composer_input_handles_empty_and_split_lines() {
    assert_eq!(wrap_composer_input("", 10), vec![String::new()]);
    assert_eq!(
        wrap_composer_input("a\nbc", 1),
        vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]
    );
}

#[test]
fn pad_display_width_keeps_width_when_short_and_long() {
    assert_eq!(pad_display_width("abc", 2), "abc");
    assert_eq!(pad_display_width("abc", 5), "abc  ");
}
