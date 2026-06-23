use ratatui::style::{Modifier, Style};
use std::{collections::VecDeque, sync::Mutex};

use super::*;

static HIGHLIGHT_CACHE_TEST_LOCK: Mutex<()> = Mutex::new(());

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
    let _cache_guard = HIGHLIGHT_CACHE_TEST_LOCK.lock().expect("cache test lock");
    highlight_cache().lock().expect("cache lock").clear();

    let spans = highlight_code_to_spans("\n", "rust").expect("blank rust line should render");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].len(), 1);
    assert_eq!(spans[0][0].content.as_ref(), "");

    let cached = cached_highlight("\n", "rust", SyntaxThemeId::CatppuccinMocha)
        .expect("highlight should be cached");
    assert_eq!(cached[0][0].content.as_ref(), "");
}

#[test]
fn cache_eviction_and_style_conversion_cover_remaining_helpers() {
    let mut cache = VecDeque::new();

    for index in 0..HIGHLIGHT_CACHE_CAPACITY {
        push_cache_entry(
            &mut cache,
            HighlightCacheEntry {
                syntax_theme: SyntaxThemeId::CatppuccinMocha,
                code: format!("code {index}"),
                language: "rust".to_owned(),
                lines: vec![vec![Span::raw(format!("line {index}"))]],
            },
        );
    }

    assert_eq!(cache.len(), HIGHLIGHT_CACHE_CAPACITY);
    assert_eq!(
        cache.front().map(|entry| entry.code.as_str()),
        Some("code 0")
    );
    assert!(cache.iter().any(|entry| {
        let expected = format!("code {}", HIGHLIGHT_CACHE_CAPACITY - 1);
        entry.code == expected
    }));

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
fn highlight_code_to_spans_prefers_cache_after_first_call() {
    let _cache_guard = HIGHLIGHT_CACHE_TEST_LOCK.lock().expect("cache test lock");
    highlight_cache().lock().expect("cache lock").clear();

    let first = highlight_code_to_spans("let a = 1;", "rust").expect("first hit");
    assert!(!first.is_empty());

    let second = highlight_code_to_spans("let a = 1;", "rust").expect("cached hit");
    assert_eq!(first.len(), second.len());

    let matching_entries = {
        let cache = highlight_cache().lock().expect("cache lock");
        cache
            .iter()
            .filter(|entry| {
                entry.syntax_theme == SyntaxThemeId::CatppuccinMocha
                    && entry.language == "rust"
                    && entry.code == "let a = 1;"
            })
            .count()
    };
    assert_eq!(matching_entries, 1);

    let mut cache = highlight_cache().lock().expect("cache lock");
    assert!(!cache.is_empty());
    cache.clear();
}

#[test]
fn highlight_cache_is_partitioned_by_syntax_theme() {
    let _cache_guard = HIGHLIGHT_CACHE_TEST_LOCK.lock().expect("cache test lock");
    highlight_cache().lock().expect("cache lock").clear();

    let code = "fn main() { let value = 1; }";
    let mocha = highlight_code_to_spans_with_theme(code, "rust", SyntaxThemeId::CatppuccinMocha)
        .expect("mocha should highlight");
    let solarized = highlight_code_to_spans_with_theme(code, "rust", SyntaxThemeId::SolarizedLight)
        .expect("solarized should highlight");

    assert_ne!(
        mocha
            .iter()
            .flat_map(|line| line.iter())
            .find(|span| span.style != Style::default())
            .map(|span| span.style),
        solarized
            .iter()
            .flat_map(|line| line.iter())
            .find(|span| span.style != Style::default())
            .map(|span| span.style)
    );
    let cache = highlight_cache().lock().expect("cache lock");
    assert!(cache.iter().any(|entry| {
        entry.syntax_theme == SyntaxThemeId::CatppuccinMocha
            && entry.language == "rust"
            && entry.code == code
    }));
    assert!(cache.iter().any(|entry| {
        entry.syntax_theme == SyntaxThemeId::SolarizedLight
            && entry.language == "rust"
            && entry.code == code
    }));
}

#[test]
fn embedded_theme_mapping_covers_all_syntax_theme_ids() {
    assert_eq!(
        embedded_theme_for(SyntaxThemeId::Auto),
        EmbeddedThemeName::CatppuccinMocha
    );
    assert_eq!(
        embedded_theme_for(SyntaxThemeId::CatppuccinMocha),
        EmbeddedThemeName::CatppuccinMocha
    );
    assert_eq!(
        embedded_theme_for(SyntaxThemeId::CatppuccinLatte),
        EmbeddedThemeName::CatppuccinLatte
    );
    assert_eq!(
        embedded_theme_for(SyntaxThemeId::SolarizedDark),
        EmbeddedThemeName::SolarizedDark
    );
    assert_eq!(
        embedded_theme_for(SyntaxThemeId::SolarizedLight),
        EmbeddedThemeName::SolarizedLight
    );
    assert_eq!(
        embedded_theme_for(SyntaxThemeId::GruvboxDark),
        EmbeddedThemeName::GruvboxDark
    );
    assert_eq!(
        embedded_theme_for(SyntaxThemeId::GruvboxLight),
        EmbeddedThemeName::GruvboxLight
    );
    assert_eq!(
        embedded_theme_for(SyntaxThemeId::Nord),
        EmbeddedThemeName::Nord
    );
    assert_eq!(
        embedded_theme_for(SyntaxThemeId::OneHalfDark),
        EmbeddedThemeName::OneHalfDark
    );
    assert_eq!(
        embedded_theme_for(SyntaxThemeId::OneHalfLight),
        EmbeddedThemeName::OneHalfLight
    );
    assert_eq!(
        embedded_theme_for(SyntaxThemeId::Monokai),
        EmbeddedThemeName::MonokaiExtended
    );
}

#[test]
fn fetch_cached_highlight_handles_missing_cache_gracefully() {
    let _cache_guard = HIGHLIGHT_CACHE_TEST_LOCK.lock().expect("cache test lock");
    highlight_cache().lock().expect("cache lock").clear();
    assert!(cached_highlight("not-found", "rust", SyntaxThemeId::CatppuccinMocha).is_none());
}

#[test]
fn cache_helpers_handle_capacity_boundaries() {
    let mut cache = VecDeque::new();

    for index in 0..HIGHLIGHT_CACHE_CAPACITY {
        push_cache_entry(
            &mut cache,
            HighlightCacheEntry {
                syntax_theme: SyntaxThemeId::CatppuccinMocha,
                code: format!("entry {index}"),
                language: "rust".to_owned(),
                lines: vec![vec![Span::raw(format!("line {index}"))]],
            },
        );
    }

    assert_eq!(cache.len(), HIGHLIGHT_CACHE_CAPACITY);
    assert_eq!(
        cache.front().map(|entry| entry.code.as_str()),
        Some("entry 0")
    );

    push_cache_entry(
        &mut cache,
        HighlightCacheEntry {
            syntax_theme: SyntaxThemeId::CatppuccinMocha,
            code: "overflow".to_owned(),
            language: "rust".to_owned(),
            lines: vec![vec![Span::raw("overflow".to_owned())]],
        },
    );

    assert_eq!(cache.len(), HIGHLIGHT_CACHE_CAPACITY);
    assert_eq!(
        cache.front().map(|entry| entry.code.as_str()),
        Some("entry 1")
    );
    assert_eq!(
        cache.back().map(|entry| entry.code.as_str()),
        Some("overflow")
    );
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

#[test]
fn syntax_lookup_supports_extension_only_and_unmatched_language() {
    assert!(find_syntax("rs").is_some());
    assert!(find_syntax("/tmp/test.rs").is_none());
    assert!(find_syntax("not-a-language").is_none());
}

#[test]
fn ansi_color_mapper_covers_remaining_palette_values() {
    assert_eq!(ansi_palette_color(0x00), RatatuiColor::Black);
    assert_eq!(ansi_palette_color(0x01), RatatuiColor::Red);
    assert_eq!(ansi_palette_color(0x02), RatatuiColor::Green);
    assert_eq!(ansi_palette_color(0x03), RatatuiColor::Yellow);
    assert_eq!(ansi_palette_color(0x04), RatatuiColor::Blue);
    assert_eq!(ansi_palette_color(0x05), RatatuiColor::Magenta);
    assert_eq!(ansi_palette_color(0x06), RatatuiColor::Cyan);
    assert_eq!(ansi_palette_color(0x07), RatatuiColor::Gray);
    assert_eq!(ansi_palette_color(0x80), RatatuiColor::Indexed(0x80));
}

#[test]
fn convert_syntect_color_covers_alpha_default_case() {
    assert_eq!(
        convert_syntect_color(SyntectColor {
            r: 12,
            g: 34,
            b: 56,
            a: ANSI_ALPHA_DEFAULT,
        }),
        None
    );
}

#[test]
fn highlight_code_to_spans_rejects_long_and_whitespace_language_inputs() {
    let long = "x".repeat(512 * 1024 + 1);
    assert!(highlight_code_to_spans(&long, "rust").is_none());

    assert!(highlight_code_to_spans("let x=1", "   ").is_none());
}
