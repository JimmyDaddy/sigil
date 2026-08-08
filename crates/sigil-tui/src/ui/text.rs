use std::borrow::Cow;

use ratatui::{
    buffer::CellWidth,
    layout::Alignment,
    style::Style,
    text::{Line, Span},
};
use unicode_segmentation::UnicodeSegmentation;

pub(crate) fn terminal_grapheme_width(grapheme: &str) -> Option<usize> {
    let sanitized = sanitized_terminal_grapheme(grapheme)?;
    let width = usize::from(sanitized.as_ref().cell_width());
    (width > 0).then_some(width)
}

pub(crate) fn terminal_cell_width(text: &str) -> usize {
    text.graphemes(true)
        .filter_map(terminal_grapheme_width)
        .sum()
}

pub(crate) fn sanitize_terminal_text(text: &str) -> String {
    text.graphemes(true)
        .fold(String::new(), |mut sanitized, grapheme| {
            if let Some(visible) = sanitized_terminal_grapheme(grapheme) {
                sanitized.push_str(visible.as_ref());
            }
            sanitized
        })
}

fn sanitized_terminal_grapheme(grapheme: &str) -> Option<Cow<'_, str>> {
    let emoji_grapheme = grapheme.chars().any(is_likely_emoji_base);
    let should_remove = |character: char| {
        character.is_control()
            || (is_default_ignorable(character)
                && !(emoji_grapheme && is_emoji_sequence_format(character)))
    };
    if !grapheme.chars().any(should_remove) {
        return (grapheme.cell_width() > 0).then_some(Cow::Borrowed(grapheme));
    }
    let visible = grapheme
        .chars()
        .filter(|character| !should_remove(*character))
        .collect::<String>();
    (!visible.is_empty() && visible.cell_width() > 0).then_some(Cow::Owned(visible))
}

fn is_default_ignorable(character: char) -> bool {
    matches!(
        character as u32,
        0x00AD
            | 0x034F
            | 0x061C
            | 0x115F..=0x1160
            | 0x17B4..=0x17B5
            | 0x180B..=0x180F
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x206F
            | 0x3164
            | 0xFE00..=0xFE0F
            | 0xFEFF
            | 0xFFA0
            | 0x1BCA0..=0x1BCAF
            | 0x1D173..=0x1D17A
            | 0xE0000..=0xE0FFF
    )
}

fn is_emoji_sequence_format(character: char) -> bool {
    matches!(
        character as u32,
        0x200D | 0xFE00..=0xFE0F | 0xE0020..=0xE007F | 0xE0100..=0xE01EF
    )
}

fn is_likely_emoji_base(character: char) -> bool {
    matches!(
        character as u32,
        0x00A9
            | 0x00AE
            | 0x203C
            | 0x2049
            | 0x2122
            | 0x2139
            | 0x2190..=0x21FF
            | 0x2300..=0x23FF
            | 0x2600..=0x27BF
            | 0x2B00..=0x2BFF
            | 0x3030
            | 0x303D
            | 0x3297
            | 0x3299
            | 0x1F000..=0x1FAFF
    )
}

pub(crate) fn truncate_inline_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let truncated = text.chars().take(max_chars).collect::<String>();
    format!("{truncated}...")
}

pub(crate) fn truncate_display_width(text: &str, max_width: usize) -> String {
    let max_width = max_width.max(1);
    let sanitized = sanitize_terminal_text(text);
    if terminal_cell_width(&sanitized) <= max_width {
        return sanitized;
    }
    let ellipsis = "...";
    let ellipsis_width = usize::from(ellipsis.cell_width());
    if max_width <= ellipsis_width {
        return ".".repeat(max_width);
    }
    let budget = max_width - ellipsis_width;
    let mut out = String::new();
    let mut used_width = 0usize;
    for grapheme in sanitized.graphemes(true) {
        let grapheme_width = terminal_grapheme_width(grapheme).unwrap_or(0);
        if used_width + grapheme_width > budget {
            break;
        }
        out.push_str(grapheme);
        used_width += grapheme_width;
    }
    format!("{out}{ellipsis}")
}

pub(crate) fn wrap_display_width(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut rows = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    let mut ended_with_line_break = false;
    for grapheme in text.graphemes(true) {
        if grapheme
            .chars()
            .any(|character| matches!(character, '\n' | '\r'))
        {
            rows.push(std::mem::take(&mut current));
            current_width = 0;
            ended_with_line_break = true;
            continue;
        }
        let Some(grapheme_width) = terminal_grapheme_width(grapheme) else {
            continue;
        };
        if grapheme_width > width {
            if !current.is_empty() {
                rows.push(std::mem::take(&mut current));
                current_width = 0;
            }
            rows.push("?".to_owned());
            ended_with_line_break = false;
            continue;
        }
        if !current.is_empty() && current_width + grapheme_width > width {
            rows.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push_str(grapheme);
        current_width += grapheme_width;
        ended_with_line_break = false;
    }
    if current.is_empty() && (rows.is_empty() || ended_with_line_break) {
        rows.push(String::new());
    } else if !current.is_empty() {
        rows.push(current);
    }
    rows
}

pub(crate) fn wrap_terminal_lines(lines: Vec<Line<'static>>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for line in lines {
        rows.extend(wrap_terminal_line(line, width));
    }
    if rows.is_empty() {
        rows.push(Line::raw(String::new()));
    }
    rows
}

fn wrap_terminal_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    let mut current_spans = Vec::new();
    let mut current_width = 0usize;
    let line_style = line.style;
    let line_alignment = line.alignment;

    for span in line.spans {
        let mut segment = String::new();
        for grapheme in span.content.as_ref().graphemes(true) {
            let Some(grapheme_width) = terminal_grapheme_width(grapheme) else {
                continue;
            };
            if current_width > 0 && current_width.saturating_add(grapheme_width) > width {
                push_styled_segment(&mut current_spans, &mut segment, span.style);
                rows.push(wrapped_line(
                    std::mem::take(&mut current_spans),
                    line_style,
                    line_alignment,
                ));
                current_width = 0;
            }
            if grapheme_width > width {
                push_styled_segment(&mut current_spans, &mut segment, span.style);
                current_spans.push(Span::styled("?", span.style));
                rows.push(wrapped_line(
                    std::mem::take(&mut current_spans),
                    line_style,
                    line_alignment,
                ));
                current_width = 0;
                continue;
            }
            segment.push_str(grapheme);
            current_width = current_width.saturating_add(grapheme_width);
        }
        push_styled_segment(&mut current_spans, &mut segment, span.style);
    }

    if !current_spans.is_empty() || rows.is_empty() {
        rows.push(wrapped_line(current_spans, line_style, line_alignment));
    }
    rows
}

fn push_styled_segment(spans: &mut Vec<Span<'static>>, segment: &mut String, style: Style) {
    if segment.is_empty() {
        return;
    }
    spans.push(Span::styled(std::mem::take(segment), style));
}

fn wrapped_line(
    spans: Vec<Span<'static>>,
    style: Style,
    alignment: Option<Alignment>,
) -> Line<'static> {
    Line {
        spans,
        style,
        alignment,
    }
}

pub(crate) fn wrap_composer_input(text: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    for line in text.split('\n') {
        rows.extend(wrap_display_width(line, width));
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

pub(crate) fn pad_display_width(text: &str, width: usize) -> String {
    let mut out = sanitize_terminal_text(text);
    let display_width = terminal_cell_width(&out);
    if width > display_width {
        out.push_str(&" ".repeat(width - display_width));
    }
    out
}

pub(crate) fn wrapped_line_rows(line: &str, width: usize) -> usize {
    if width == 0 {
        return 1;
    }
    let display_width = terminal_cell_width(line);
    if display_width == 0 {
        return 1;
    }
    display_width.div_ceil(width)
}

pub(crate) fn visual_position_for_char_cursor(
    input: &str,
    cursor: usize,
    width: usize,
) -> (usize, usize) {
    let width = width.max(1);
    let mut row = 0usize;
    let mut column = 0usize;
    let mut char_index = 0usize;
    for grapheme in input.graphemes(true) {
        let grapheme_chars = grapheme.chars().count();
        let grapheme_end = char_index.saturating_add(grapheme_chars);
        if cursor < grapheme_end {
            break;
        }
        char_index = grapheme_end;
        if grapheme == "\n" {
            row = row.saturating_add(1);
            column = 0;
            continue;
        }
        let Some(grapheme_width) = terminal_grapheme_width(grapheme) else {
            continue;
        };
        if column > 0 && column.saturating_add(grapheme_width) > width {
            row = row.saturating_add(1);
            column = 0;
        }
        if grapheme_width >= width || column.saturating_add(grapheme_width) == width {
            row = row.saturating_add(1);
            column = 0;
        } else {
            column = column.saturating_add(grapheme_width);
        }
    }
    (row, column)
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/text_tests.rs"]
mod tests;
