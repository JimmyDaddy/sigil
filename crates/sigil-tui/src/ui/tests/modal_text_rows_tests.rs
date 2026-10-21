use super::*;
use crate::ui::text::terminal_cell_width;
use ratatui::style::Color;

#[test]
fn modal_word_rows_preserve_words_across_styles_spaces_lf_and_blank_lines() {
    let red = Style::default().fg(Color::Red);
    let blue = Style::default().fg(Color::Blue);
    let rows = word_wrap_rows(
        vec![Line::from(vec![
            Span::raw("alpha "),
            Span::styled("be", red),
            Span::styled("ta", blue),
            Span::raw("  gamma"),
        ])],
        9,
    );
    assert_eq!(
        rows.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["alpha ", "beta  ", "gamma"]
    );
    assert_eq!(rows[1].spans[0].style, red);
    assert_eq!(rows[1].spans[1].style, blue);
    assert_eq!(
        rows.iter().map(ToString::to_string).collect::<String>(),
        "alpha beta  gamma"
    );
    let source = "first\n\nline  A👩🏽‍💻e\u{301}TAIL\n";
    let rows = word_wrap_rows(vec![Line::from(Span::raw(source))], 100);
    assert_eq!(
        rows.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["first", "", "line  A👩🏽‍💻e\u{301}TAIL", ""]
    );
}

#[test]
fn modal_word_rows_keep_long_tokens_and_extended_graphemes_complete() {
    let source = format!("/{}END", "中文e\u{301}👩🏽‍💻/".repeat(60));
    let rows = word_wrap_rows(vec![Line::from(Span::raw(source.clone()))], 11);
    assert!(rows.len() > 20);
    assert!(
        rows.iter()
            .all(|row| terminal_cell_width(&row.to_string()) <= 11)
    );
    assert_eq!(
        rows.iter().map(ToString::to_string).collect::<String>(),
        source
    );
    for row in rows {
        assert!(!row.to_string().starts_with('\u{301}'));
        assert!(!row.to_string().ends_with('\u{200d}'));
    }
}
