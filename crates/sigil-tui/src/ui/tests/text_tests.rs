use super::*;
use ratatui::style::Color;

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
    assert_eq!(truncate_display_width("你好", 2), "..");
    assert_eq!(truncate_display_width("你好", 5), "你好");
    assert_eq!(truncate_display_width("abc", 1), ".");
    for width in 1..=6 {
        assert!(terminal_cell_width(&truncate_display_width("👨‍👩‍👧‍👦abcdef", width)) <= width);
    }
}

#[test]
fn composer_cursor_width_uses_the_same_graphemes_as_rendering() {
    let family = "👨‍👩‍👧‍👦";
    assert_eq!(
        visual_position_for_char_cursor(family, family.chars().count(), 10),
        (0, 2)
    );
    assert_eq!(
        visual_position_for_char_cursor(family, family.chars().count(), 2),
        (1, 0)
    );
    assert_eq!(visual_position_for_char_cursor("ｶﾞ", 2, 4), (0, 2));
    assert_eq!(wrap_composer_input("ｶﾞ", 2), vec!["ｶﾞ"]);
}

#[test]
fn wrap_display_width_preserves_empty_and_multichar_lines() {
    assert_eq!(wrap_display_width("", 10), vec![String::from("")]);
    assert_eq!(
        wrap_display_width("abcdef", 3),
        vec!["abc".to_owned(), "def".to_owned()]
    );
    assert_eq!(
        wrap_display_width("ab\ncd", 10),
        vec!["ab".to_owned(), "cd".to_owned()]
    );
    assert_eq!(terminal_cell_width("a\nb"), 2);
    assert_eq!(wrap_display_width("a\tb", 10), vec!["ab".to_owned()]);
    assert_eq!(visual_position_for_char_cursor("a\tb", 3, 10), (0, 2));
}

#[test]
fn terminal_text_drops_standalone_zero_width_graphemes_but_keeps_joined_ones() {
    let family = "👨‍👩‍👧‍👦";
    assert_eq!(sanitize_terminal_text("a\u{200b}b"), "ab");
    assert_eq!(sanitize_terminal_text("k\u{200d}"), "k");
    assert_eq!(sanitize_terminal_text("\u{301}"), "");
    assert_eq!(sanitize_terminal_text("e\u{301}"), "e\u{301}");
    assert_eq!(sanitize_terminal_text(family), family);
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
fn one_cell_rows_replace_unrenderable_wide_graphemes_without_phantom_rows() {
    assert_eq!(wrap_display_width("你", 1), vec!["?".to_owned()]);
    assert_eq!(wrap_composer_input("你", 1), vec!["?".to_owned()]);

    let rows = wrap_terminal_lines(vec![Line::raw("你")], 1);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].spans[0].content.as_ref(), "?");
}

#[test]
fn terminal_line_wrapping_preserves_embedded_line_breaks() {
    let rows = wrap_terminal_lines(
        vec![Line::from(vec![
            Span::styled("alpha\nbeta\n", Style::default().fg(Color::Blue)),
            Span::styled("gamma", Style::default().fg(Color::Green)),
        ])],
        20,
    );
    let values = rows
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert_eq!(values, vec!["alpha", "beta", "gamma"]);
    assert_eq!(rows[0].spans[0].style.fg, Some(Color::Blue));
    assert_eq!(rows[2].spans[0].style.fg, Some(Color::Green));
}

#[test]
fn pad_display_width_keeps_width_when_short_and_long() {
    assert_eq!(pad_display_width("abc", 2), "abc");
    assert_eq!(pad_display_width("abc", 5), "abc  ");
    assert_eq!(pad_display_width("a\tb", 3), "ab ");
}
