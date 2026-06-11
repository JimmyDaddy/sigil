use ratatui::style::{Modifier, Style};
use std::collections::VecDeque;

use super::*;

fn plain_text(spans: &[Vec<Span<'static>>]) -> String {
    spans
        .iter()
        .flat_map(|line| line.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

#[test]
fn highlights_known_language_into_token_spans() {
    let spans = highlight_code_to_spans("fn main() {\n    let value = 1;\n}", "rust")
        .expect("rust should resolve");

    assert_eq!(plain_text(&spans), "fn main() {    let value = 1;}");
    assert!(
        spans
            .iter()
            .flat_map(|line| line.iter())
            .any(|span| span.content.as_ref() == "fn" && span.style != Style::default())
    );
}

#[test]
fn resolves_common_language_aliases() {
    for language in ["rs", "sh", "shell", "py", "python3", "ts", "js", "yml"] {
        assert!(
            highlight_code_to_spans("let x = 1;", language).is_some(),
            "{language} should resolve"
        );
    }
}

#[test]
fn returns_none_for_unknown_language() {
    assert!(highlight_code_to_spans("content", "not-a-real-language").is_none());
}

#[test]
fn strips_crlf_line_endings_from_spans() {
    let spans = highlight_code_to_spans("fn main() {}\r\nlet value = 1;\r\n", "rust")
        .expect("rust should resolve");

    assert!(
        !spans
            .iter()
            .flat_map(|line| line.iter())
            .any(|span| span.content.contains('\r') || span.content.contains('\n'))
    );
}

#[test]
fn refuses_oversized_inputs() {
    let too_large = "x".repeat(512 * 1024 + 1);
    assert!(highlight_code_to_spans(&too_large, "rust").is_none());

    let too_many_lines = "let x = 1;\n".repeat(10_001);
    assert!(highlight_code_to_spans(&too_many_lines, "rust").is_none());
}

#[test]
fn blank_lines_and_cache_hits_cover_internal_paths() {
    highlight_cache().lock().expect("cache lock").clear();

    let spans = highlight_code_to_spans("\n", "rust").expect("blank rust line should render");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].len(), 1);
    assert_eq!(spans[0][0].content.as_ref(), "");

    let cached = cached_highlight("\n", "rust").expect("highlight should be cached");
    assert_eq!(cached[0][0].content.as_ref(), "");
}

#[test]
fn cache_eviction_and_style_conversion_cover_remaining_helpers() {
    let mut cache = VecDeque::new();

    for index in 0..=HIGHLIGHT_CACHE_CAPACITY {
        push_cache_entry(
            &mut cache,
            HighlightCacheEntry {
                code: format!("code {index}"),
                language: "rust".to_owned(),
                lines: vec![vec![Span::raw(format!("line {index}"))]],
            },
        );
    }

    assert_eq!(cache.len(), HIGHLIGHT_CACHE_CAPACITY);
    assert!(cache.iter().all(|entry| entry.code != "code 0"));
    assert!(
        cache
            .iter()
            .any(|entry| entry.code == format!("code {HIGHLIGHT_CACHE_CAPACITY}"))
    );

    assert_eq!(ansi_palette_color(0x07), RatatuiColor::Gray);
    assert_eq!(ansi_palette_color(0x42), RatatuiColor::Indexed(0x42));
    assert_eq!(
        convert_syntect_color(SyntectColor {
            r: 0x04,
            g: 0,
            b: 0,
            a: ANSI_ALPHA_INDEX,
        }),
        Some(RatatuiColor::Blue)
    );
    assert_eq!(
        convert_syntect_color(SyntectColor {
            r: 1,
            g: 2,
            b: 3,
            a: ANSI_ALPHA_DEFAULT,
        }),
        None
    );
    assert_eq!(
        convert_syntect_color(SyntectColor {
            r: 1,
            g: 2,
            b: 3,
            a: OPAQUE_ALPHA,
        }),
        Some(RatatuiColor::Rgb(1, 2, 3))
    );
    assert_eq!(
        convert_syntect_color(SyntectColor {
            r: 4,
            g: 5,
            b: 6,
            a: 0x7F,
        }),
        Some(RatatuiColor::Rgb(4, 5, 6))
    );

    let style = convert_style(SyntectStyle {
        foreground: SyntectColor {
            r: 0x03,
            g: 0,
            b: 0,
            a: ANSI_ALPHA_INDEX,
        },
        background: SyntectColor {
            r: 0,
            g: 0,
            b: 0,
            a: OPAQUE_ALPHA,
        },
        font_style: FontStyle::BOLD,
    });
    assert_eq!(style.fg, Some(RatatuiColor::Yellow));
    assert!(style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn syntax_lookup_covers_alias_name_case_and_extension_fallbacks() {
    assert_eq!(normalized_language_token("csharp"), "c#");
    assert_eq!(normalized_language_token("golang snippet"), "go");
    assert_eq!(find_syntax("Rust").expect("exact name").name, "Rust");
    assert_eq!(
        find_syntax("RUST").expect("lowercase fallback").name,
        "Rust"
    );
    assert!(syntax_set().find_syntax_by_extension("rs").is_some());
    assert!(find_syntax("rs").is_some());
}
