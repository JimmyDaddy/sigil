use std::{
    collections::VecDeque,
    sync::{Mutex, OnceLock},
};

use ratatui::{
    style::{Color as RatatuiColor, Modifier, Style},
    text::Span,
};
use syntect::{
    easy::HighlightLines,
    highlighting::{Color as SyntectColor, FontStyle, Style as SyntectStyle, Theme},
    parsing::{SyntaxReference, SyntaxSet},
    util::LinesWithEndings,
};
use two_face::theme::EmbeddedThemeName;

static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
static THEME: OnceLock<Theme> = OnceLock::new();
static HIGHLIGHT_CACHE: OnceLock<Mutex<VecDeque<HighlightCacheEntry>>> = OnceLock::new();

const ANSI_ALPHA_INDEX: u8 = 0x00;
const ANSI_ALPHA_DEFAULT: u8 = 0x01;
const OPAQUE_ALPHA: u8 = 0xFF;
const MAX_HIGHLIGHT_BYTES: usize = 512 * 1024;
const MAX_HIGHLIGHT_LINES: usize = 10_000;
const HIGHLIGHT_CACHE_CAPACITY: usize = 96;

type HighlightedLines = Vec<Vec<Span<'static>>>;

#[derive(Clone)]
struct HighlightCacheEntry {
    language: String,
    code: String,
    lines: HighlightedLines,
}

fn syntax_set() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(two_face::syntax::extra_newlines)
}

fn theme() -> &'static Theme {
    THEME.get_or_init(|| {
        two_face::theme::extra()
            .get(EmbeddedThemeName::CatppuccinMocha)
            .clone()
    })
}

pub(crate) fn highlight_code_to_spans(code: &str, language: &str) -> Option<HighlightedLines> {
    if code.is_empty()
        || language.trim().is_empty()
        || code.len() > MAX_HIGHLIGHT_BYTES
        || code.lines().count() > MAX_HIGHLIGHT_LINES
    {
        return None;
    }
    let language = normalized_language_token(language);
    if language.is_empty() {
        return None;
    }
    if let Some(lines) = cached_highlight(code, language) {
        return Some(lines);
    }

    let syntax = find_syntax(language)?;
    let mut highlighter = HighlightLines::new(syntax, theme());
    let mut lines = Vec::new();

    for line in LinesWithEndings::from(code) {
        let ranges = highlighter.highlight_line(line, syntax_set()).ok()?;
        let mut spans = Vec::new();
        for (style, text) in ranges {
            let text = text.trim_end_matches(['\n', '\r']);
            if !text.is_empty() {
                spans.push(Span::styled(text.to_owned(), convert_style(style)));
            }
        }
        if spans.is_empty() {
            spans.push(Span::raw(String::new()));
        }
        lines.push(spans);
    }

    cache_highlight(code, language, lines.clone());
    Some(lines)
}

fn highlight_cache() -> &'static Mutex<VecDeque<HighlightCacheEntry>> {
    HIGHLIGHT_CACHE.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn cached_highlight(code: &str, language: &str) -> Option<HighlightedLines> {
    let mut cache = match highlight_cache().lock() {
        Ok(cache) => cache,
        Err(poisoned) => poisoned.into_inner(),
    };
    let position = cache
        .iter()
        .position(|entry| entry.language == language && entry.code == code)?;
    let entry = cache.remove(position)?;
    let lines = entry.lines.clone();
    cache.push_back(entry);
    Some(lines)
}

fn cache_highlight(code: &str, language: &str, lines: HighlightedLines) {
    let mut cache = match highlight_cache().lock() {
        Ok(cache) => cache,
        Err(poisoned) => poisoned.into_inner(),
    };
    if cache.len() >= HIGHLIGHT_CACHE_CAPACITY {
        cache.pop_front();
    }
    cache.push_back(HighlightCacheEntry {
        language: language.to_owned(),
        code: code.to_owned(),
        lines,
    });
}

fn find_syntax(language: &str) -> Option<&'static SyntaxReference> {
    let syntax_set = syntax_set();
    let token = normalized_language_token(language);

    if let Some(syntax) = syntax_set.find_syntax_by_token(token) {
        return Some(syntax);
    }
    if let Some(syntax) = syntax_set.find_syntax_by_name(token) {
        return Some(syntax);
    }
    let lower = token.to_ascii_lowercase();
    if let Some(syntax) = syntax_set
        .syntaxes()
        .iter()
        .find(|syntax| syntax.name.to_ascii_lowercase() == lower)
    {
        return Some(syntax);
    }
    syntax_set.find_syntax_by_extension(token)
}

fn normalized_language_token(language: &str) -> &str {
    let token = language.split_whitespace().next().unwrap_or(language);
    match token {
        "c-sharp" | "csharp" => "c#",
        "golang" => "go",
        "js" => "javascript",
        "py" | "python3" => "python",
        "rs" => "rust",
        "sh" | "shell" => "bash",
        "ts" => "typescript",
        "yml" => "yaml",
        _ => token,
    }
}

#[allow(clippy::disallowed_methods)]
fn ansi_palette_color(index: u8) -> RatatuiColor {
    match index {
        0x00 => RatatuiColor::Black,
        0x01 => RatatuiColor::Red,
        0x02 => RatatuiColor::Green,
        0x03 => RatatuiColor::Yellow,
        0x04 => RatatuiColor::Blue,
        0x05 => RatatuiColor::Magenta,
        0x06 => RatatuiColor::Cyan,
        0x07 => RatatuiColor::Gray,
        color => RatatuiColor::Indexed(color),
    }
}

#[allow(clippy::disallowed_methods)]
fn convert_syntect_color(color: SyntectColor) -> Option<RatatuiColor> {
    match color.a {
        ANSI_ALPHA_INDEX => Some(ansi_palette_color(color.r)),
        ANSI_ALPHA_DEFAULT => None,
        OPAQUE_ALPHA => Some(RatatuiColor::Rgb(color.r, color.g, color.b)),
        _ => Some(RatatuiColor::Rgb(color.r, color.g, color.b)),
    }
}

fn convert_style(style: SyntectStyle) -> Style {
    let mut rendered = Style::default();
    if let Some(fg) = convert_syntect_color(style.foreground) {
        rendered = rendered.fg(fg);
    }
    if style.font_style.contains(FontStyle::BOLD) {
        rendered = rendered.add_modifier(Modifier::BOLD);
    }
    rendered
}

#[cfg(test)]
#[path = "tests/syntax_highlight_tests.rs"]
mod tests;
