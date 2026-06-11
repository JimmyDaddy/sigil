use ratatui::style::Style;

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
