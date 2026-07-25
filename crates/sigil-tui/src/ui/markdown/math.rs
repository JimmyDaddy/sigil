use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use super::super::{
    primitives::{timeline_content_line, timeline_section_line_with_palette},
    text::{truncate_display_width, wrap_display_width},
    theme::ThemePalette,
};

pub(super) fn inline_math(text: &str) -> Option<(&str, usize)> {
    let after = text.strip_prefix('$')?;
    if after.starts_with('$') {
        return None;
    }
    let mut escaped = false;
    for (index, character) in after.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if character == '$' && index > 0 {
            return Some((&text[..index + 2], index + 2));
        }
    }
    None
}

pub(super) fn display_math_block<'a>(
    lines: &'a [&'a str],
    start: usize,
) -> Option<(usize, Vec<&'a str>)> {
    let opening = lines.get(start)?.trim();
    if !opening.starts_with("$$") {
        return None;
    }
    if opening.len() > 4 && opening.ends_with("$$") {
        let source = lines[start].trim();
        return Some((start + 1, vec![source]));
    }
    if opening != "$$" {
        return None;
    }
    let mut block = vec![lines[start]];
    let mut index = start + 1;
    while index < lines.len() {
        block.push(lines[index]);
        index += 1;
        if block.last().is_some_and(|line| line.trim() == "$$") {
            return Some((index, block));
        }
    }
    None
}

pub(super) fn render_display_math(
    accent: ratatui::style::Color,
    source_lines: &[&str],
    max_content_width: usize,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let mut lines = vec![timeline_section_line_with_palette(
        accent,
        "formula",
        palette.markdown_math,
        Vec::new(),
        palette,
    )];
    lines.push(timeline_content_line(
        accent,
        vec![Span::styled(
            truncate_display_width("LaTeX source", max_content_width.saturating_sub(2)),
            Style::default().fg(palette.text_muted),
        )],
    ));
    let source = source_lines.join("\n");
    for row in wrap_display_width(&source, max_content_width.saturating_sub(4).max(1)) {
        lines.push(timeline_content_line(
            accent,
            vec![
                Span::styled(
                    "∑ ",
                    Style::default()
                        .fg(palette.markdown_math)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(row, Style::default().fg(palette.markdown_math)),
            ],
        ));
    }
    lines
}
