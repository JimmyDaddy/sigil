use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use sigil_kernel::SyntaxThemeId;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::{
    primitives::{timeline_content_line, timeline_section_line_with_palette},
    syntax_highlight::highlight_code_to_spans_with_theme,
    text::{pad_display_width, truncate_display_width, wrap_display_width},
    theme::{self, ThemePalette},
};

#[derive(Default)]
pub(crate) struct MarkdownRenderState {
    in_fenced_code: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MarkdownRenderOptions {
    pub max_content_width: usize,
    pub code_wrap: CodeWrapMode,
    pub highlight_code: bool,
    pub syntax_theme: SyntaxThemeId,
    pub show_link_urls: bool,
    pub table_mode: TableRenderMode,
}

impl MarkdownRenderOptions {
    pub(crate) fn timeline(max_content_width: usize) -> Self {
        let max_content_width = if max_content_width == 0 {
            80
        } else {
            max_content_width
        };
        Self {
            max_content_width,
            code_wrap: CodeWrapMode::Preserve,
            highlight_code: true,
            syntax_theme: SyntaxThemeId::default(),
            show_link_urls: true,
            table_mode: TableRenderMode::Compact,
        }
        .normalized()
    }

    pub(crate) fn tool_preview(max_content_width: usize) -> Self {
        Self::timeline(max_content_width)
    }

    pub(crate) fn modal(max_content_width: usize) -> Self {
        Self::timeline(max_content_width)
    }

    fn normalized(mut self) -> Self {
        self.max_content_width = self.max_content_width.max(20);
        self
    }

    pub(crate) fn with_syntax_theme(mut self, syntax_theme: SyntaxThemeId) -> Self {
        self.syntax_theme = syntax_theme;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodeWrapMode {
    Preserve,
    #[cfg(test)]
    Wrap,
    #[cfg(test)]
    Truncate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TableRenderMode {
    Compact,
    #[cfg(test)]
    Preserve,
}

#[allow(dead_code)]
pub(crate) fn render_markdown_timeline_lines(
    accent: Color,
    body_style: Style,
    text: &str,
    options: MarkdownRenderOptions,
) -> Vec<Line<'static>> {
    let palette = theme::default_palette();
    render_markdown_timeline_lines_with_palette(accent, body_style, text, options, &palette)
}

pub(crate) fn render_markdown_timeline_lines_with_palette(
    accent: Color,
    body_style: Style,
    text: &str,
    options: MarkdownRenderOptions,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let source_lines = text.lines().collect::<Vec<_>>();
    let mut rendered = Vec::new();
    let options = options.normalized();
    let mut index = 0usize;
    while index < source_lines.len() {
        let line = source_lines[index];
        if line.trim().is_empty() {
            if rendered
                .last()
                .map(|line: &Line<'static>| !line.spans.is_empty())
                .unwrap_or(false)
            {
                rendered.push(Line::raw(String::new()));
            }
            index += 1;
            continue;
        }
        if let Some((level, content)) = markdown_heading(line) {
            rendered.extend(render_markdown_heading_block_with_palette(
                level, content, body_style, options, palette,
            ));
            index += 1;
            continue;
        }
        if let Some(language) = fenced_code_language(line) {
            let language_token = markdown_code_language_token(language);
            let label = if language_token.is_empty() {
                "plain"
            } else {
                language_token
            };
            index += 1;
            let mut block_lines = Vec::new();
            while index < source_lines.len() {
                if fenced_code_language(source_lines[index]).is_some() {
                    index += 1;
                    break;
                }
                block_lines.push(source_lines[index]);
                index += 1;
            }
            rendered.push(timeline_section_line_with_palette(
                accent,
                "code",
                palette.accent_info,
                vec![Span::styled(
                    label.to_owned(),
                    Style::default().fg(palette.text_muted),
                )],
                palette,
            ));
            if block_lines.is_empty() {
                rendered.push(timeline_content_line(
                    accent,
                    render_code_line_spans_with_bg(
                        "",
                        palette.accent_info,
                        Style::default().fg(palette.markdown_code_fg),
                        palette.markdown_code_bg,
                    ),
                ));
            } else {
                let highlighted = highlight_code_block_lines(&block_lines, language_token, options);
                for (line_index, block_line) in block_lines.iter().enumerate() {
                    rendered.extend(render_code_block_line_rows(
                        accent,
                        block_line,
                        highlighted
                            .as_ref()
                            .and_then(|highlighted| highlighted.get(line_index))
                            .map(Vec::as_slice),
                        options,
                        palette,
                    ));
                }
            }
            continue;
        }
        if markdown_table_line(line) {
            let start = index;
            while index < source_lines.len() && markdown_table_line(source_lines[index]) {
                index += 1;
            }
            rendered.extend(render_markdown_table_block_with_palette(
                accent,
                body_style,
                &source_lines[start..index],
                options,
                palette,
            ));
            continue;
        }
        if markdown_quote(line).is_some() {
            let start = index;
            while index < source_lines.len() && markdown_quote(source_lines[index]).is_some() {
                index += 1;
            }
            rendered.push(timeline_section_line_with_palette(
                accent,
                "quote",
                palette.markdown_quote_bar,
                vec![Span::styled(
                    "quoted context",
                    Style::default().fg(palette.text_muted),
                )],
                palette,
            ));
            for quote_line in &source_lines[start..index] {
                let content = markdown_quote(quote_line).unwrap_or_else(|| quote_line.trim());
                rendered.extend(render_wrapped_content_lines(
                    accent,
                    quote_prefix_spans(palette),
                    quote_prefix_spans(palette),
                    render_inline_markdown_spans_with_palette(
                        content,
                        body_style.fg(palette.markdown_quote_text),
                        options,
                        palette,
                    ),
                    options,
                ));
            }
            continue;
        }

        rendered.extend(render_wrapped_markdown_line_with_palette(
            accent, line, body_style, options, palette,
        ));
        index += 1;
    }
    rendered
}

fn code_block_render_rows(line: &str, options: MarkdownRenderOptions) -> Vec<String> {
    let width = options.max_content_width.saturating_sub(2).max(1);
    match options.code_wrap {
        CodeWrapMode::Preserve => {
            if UnicodeWidthStr::width(line) > width {
                wrap_display_width(line, width)
            } else {
                vec![line.to_owned()]
            }
        }
        #[cfg(test)]
        CodeWrapMode::Wrap => wrap_display_width(line, width),
        #[cfg(test)]
        CodeWrapMode::Truncate => vec![truncate_display_width(line, width)],
    }
}

fn highlight_code_block_lines(
    block_lines: &[&str],
    language: &str,
    options: MarkdownRenderOptions,
) -> Option<Vec<Vec<Span<'static>>>> {
    if !options.highlight_code {
        return None;
    }
    highlight_code_to_spans_with_theme(&block_lines.join("\n"), language, options.syntax_theme)
}

fn render_code_block_line_rows(
    accent: Color,
    block_line: &str,
    highlighted_spans: Option<&[Span<'static>]>,
    options: MarkdownRenderOptions,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let rows = code_block_render_rows(block_line, options);
    if rows.len() == 1
        && rows.first().is_some_and(|row| row == block_line)
        && let Some(spans) = highlighted_spans
    {
        return vec![timeline_content_line(
            accent,
            render_highlighted_code_line_spans(
                spans,
                palette.accent_info,
                Style::default().fg(palette.markdown_code_fg),
                palette.markdown_code_bg,
            ),
        )];
    }

    rows.into_iter()
        .map(|code_row| {
            timeline_content_line(
                accent,
                render_code_line_spans_with_bg(
                    &code_row,
                    palette.accent_info,
                    Style::default().fg(palette.markdown_code_fg),
                    palette.markdown_code_bg,
                ),
            )
        })
        .collect()
}

#[cfg(test)]
fn render_wrapped_markdown_line(
    accent: Color,
    line: &str,
    body_style: Style,
    options: MarkdownRenderOptions,
) -> Vec<Line<'static>> {
    let palette = theme::default_palette();
    render_wrapped_markdown_line_with_palette(accent, line, body_style, options, &palette)
}

fn render_wrapped_markdown_line_with_palette(
    accent: Color,
    line: &str,
    body_style: Style,
    options: MarkdownRenderOptions,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let options = options.normalized();
    if markdown_rule(line) {
        return vec![timeline_content_line(
            accent,
            vec![Span::styled(
                "────────────────────────────────",
                Style::default().fg(palette.markdown_rule),
            )],
        )];
    }
    if let Some((checked, content)) = markdown_task_item(line) {
        let marker = if checked { "[x]" } else { "[ ]" };
        let indent = markdown_list_indent(line);
        let prefix = vec![Span::styled(
            format!("{}{marker} ", "  ".repeat(indent)),
            Style::default()
                .fg(if checked {
                    palette.accent_success
                } else {
                    palette.accent_warning
                })
                .add_modifier(Modifier::BOLD),
        )];
        return render_wrapped_content_lines(
            accent,
            prefix.clone(),
            continuation_indent_spans(&prefix, body_style),
            render_inline_markdown_spans_with_palette(content, body_style, options, palette),
            options,
        );
    }
    if let Some(content) = markdown_bullet_item(line) {
        let indent = markdown_list_indent(line);
        let prefix = vec![Span::styled(
            format!("{}• ", "  ".repeat(indent)),
            Style::default()
                .fg(palette.accent_warning)
                .add_modifier(Modifier::BOLD),
        )];
        return render_wrapped_content_lines(
            accent,
            prefix.clone(),
            continuation_indent_spans(&prefix, body_style),
            render_inline_markdown_spans_with_palette(content, body_style, options, palette),
            options,
        );
    }
    if let Some((number, content)) = markdown_ordered_item(line) {
        let indent = markdown_list_indent(line);
        let prefix = vec![Span::styled(
            format!("{}{number}. ", "  ".repeat(indent)),
            Style::default()
                .fg(palette.accent_warning)
                .add_modifier(Modifier::BOLD),
        )];
        return render_wrapped_content_lines(
            accent,
            prefix.clone(),
            continuation_indent_spans(&prefix, body_style),
            render_inline_markdown_spans_with_palette(content, body_style, options, palette),
            options,
        );
    }
    if let Some(content) = markdown_quote(line) {
        return render_wrapped_content_lines(
            accent,
            quote_prefix_spans(palette),
            quote_prefix_spans(palette),
            render_inline_markdown_spans_with_palette(
                content,
                body_style.fg(palette.markdown_quote_text),
                options,
                palette,
            ),
            options,
        );
    }
    if line_looks_like_code(line) {
        return code_block_render_rows(line, options)
            .into_iter()
            .map(|row| {
                timeline_content_line(
                    accent,
                    render_code_line_spans_with_bg(
                        &row,
                        palette.accent_info,
                        Style::default().fg(palette.markdown_code_fg),
                        palette.markdown_code_bg,
                    ),
                )
            })
            .collect();
    }
    render_wrapped_content_lines(
        accent,
        Vec::new(),
        Vec::new(),
        render_inline_markdown_spans_with_palette(line, body_style, options, palette),
        options,
    )
}

fn render_wrapped_content_lines(
    accent: Color,
    first_prefix: Vec<Span<'static>>,
    continuation_prefix: Vec<Span<'static>>,
    content: Vec<Span<'static>>,
    options: MarkdownRenderOptions,
) -> Vec<Line<'static>> {
    wrap_prefixed_spans(
        first_prefix,
        continuation_prefix,
        content,
        options.max_content_width,
    )
    .into_iter()
    .map(|spans| timeline_content_line(accent, spans))
    .collect()
}

fn wrap_prefixed_spans(
    first_prefix: Vec<Span<'static>>,
    continuation_prefix: Vec<Span<'static>>,
    content: Vec<Span<'static>>,
    max_width: usize,
) -> Vec<Vec<Span<'static>>> {
    let max_width = max_width.max(1);
    let continuation_width = spans_display_width(&continuation_prefix);
    let mut rows = Vec::new();
    let mut current = first_prefix;
    let mut current_width = spans_display_width(&current);
    let mut current_has_content = false;

    for span in content {
        for grapheme in span.content.as_ref().graphemes(true) {
            let grapheme_width = UnicodeWidthStr::width(grapheme).max(1);
            if current_has_content && current_width + grapheme_width > max_width {
                rows.push(current);
                current = continuation_prefix.clone();
                current_width = continuation_width;
            }
            push_styled_text(&mut current, grapheme, span.style);
            current_width += grapheme_width;
            current_has_content = true;
        }
    }

    if current_has_content || rows.is_empty() {
        rows.push(current);
    }
    rows
}

fn push_styled_text(spans: &mut Vec<Span<'static>>, text: &str, style: Style) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut()
        && last.style == style
    {
        last.content.to_mut().push_str(text);
        return;
    }
    spans.push(Span::styled(text.to_owned(), style));
}

fn continuation_indent_spans(prefix: &[Span<'static>], style: Style) -> Vec<Span<'static>> {
    vec![Span::styled(" ".repeat(spans_display_width(prefix)), style)]
}

fn quote_prefix_spans(palette: &ThemePalette) -> Vec<Span<'static>> {
    vec![Span::styled(
        "▌ ",
        Style::default()
            .fg(palette.markdown_quote_bar)
            .add_modifier(Modifier::BOLD),
    )]
}

fn spans_display_width(spans: &[Span<'static>]) -> usize {
    spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

#[cfg(test)]
fn render_markdown_heading_block(
    level: usize,
    content: &str,
    base_style: Style,
    options: MarkdownRenderOptions,
) -> Vec<Line<'static>> {
    let palette = theme::default_palette();
    render_markdown_heading_block_with_palette(level, content, base_style, options, &palette)
}

fn render_markdown_heading_block_with_palette(
    level: usize,
    content: &str,
    base_style: Style,
    options: MarkdownRenderOptions,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let accent = match level {
        1 => palette.markdown_heading,
        2 => palette.accent_info,
        3 => palette.accent_success,
        _ => palette.accent_secondary,
    };
    let title_spans = render_inline_markdown_spans_with_palette(
        content,
        base_style.fg(accent).add_modifier(Modifier::BOLD),
        options,
        palette,
    );
    let mut lines = vec![Line::from(title_spans)];
    if level <= 2 {
        let underline_width =
            UnicodeWidthStr::width(content).clamp(8, options.max_content_width.max(8));
        lines.push(Line::from(vec![Span::styled(
            "─".repeat(underline_width),
            Style::default().fg(palette.markdown_rule),
        )]));
    }
    lines
}

#[cfg(test)]
fn render_markdown_table_block(
    accent: Color,
    body_style: Style,
    rows: &[&str],
    options: MarkdownRenderOptions,
) -> Vec<Line<'static>> {
    let palette = theme::default_palette();
    render_markdown_table_block_with_palette(accent, body_style, rows, options, &palette)
}

fn render_markdown_table_block_with_palette(
    accent: Color,
    body_style: Style,
    rows: &[&str],
    options: MarkdownRenderOptions,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    if rows.is_empty() {
        return Vec::new();
    }

    let parsed_rows = rows
        .iter()
        .map(|line| {
            markdown_table_cells(line)
                .into_iter()
                .map(|cell| markdown_plain_text(&cell))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let header = parsed_rows.first().cloned().unwrap_or_default();
    let has_divider = parsed_rows
        .get(1)
        .is_some_and(|row| row.iter().all(|cell| markdown_table_divider_cell(cell)));
    let body_rows = if has_divider {
        parsed_rows.iter().skip(2).cloned().collect::<Vec<_>>()
    } else {
        parsed_rows.iter().skip(1).cloned().collect::<Vec<_>>()
    };
    let column_count = parsed_rows.iter().map(Vec::len).max().unwrap_or(0);
    if column_count == 0 {
        return Vec::new();
    }

    let natural_widths = (0..column_count)
        .map(|column| {
            parsed_rows
                .iter()
                .filter_map(|row| row.get(column))
                .filter(|cell| !markdown_table_divider_cell(cell))
                .map(|cell| UnicodeWidthStr::width(cell.as_str()))
                .max()
                .unwrap_or(3)
                .max(3)
        })
        .collect::<Vec<_>>();
    let widths = match options.table_mode {
        TableRenderMode::Compact => {
            clamp_table_widths(&natural_widths, options.max_content_width.max(24))
        }
        #[cfg(test)]
        TableRenderMode::Preserve => natural_widths,
    };

    let summary = format!(
        "{} cols · {} rows",
        column_count,
        body_rows.len().saturating_add(1)
    );
    let mut lines = vec![timeline_section_line_with_palette(
        accent,
        "table",
        palette.accent_secondary,
        vec![Span::styled(
            summary,
            Style::default().fg(palette.text_muted),
        )],
        palette,
    )];

    lines.push(timeline_content_line(
        accent,
        vec![Span::styled(
            markdown_table_border(&widths, '┌', '┬', '┐', '─'),
            Style::default().fg(palette.markdown_rule),
        )],
    ));
    for header_line in markdown_table_row_lines(&header, &widths) {
        lines.push(timeline_content_line(
            accent,
            vec![Span::styled(
                header_line,
                body_style
                    .fg(palette.accent_info)
                    .add_modifier(Modifier::BOLD),
            )],
        ));
    }
    lines.push(timeline_content_line(
        accent,
        vec![Span::styled(
            markdown_table_border(&widths, '├', '┼', '┤', if has_divider { '═' } else { '─' }),
            Style::default().fg(palette.markdown_rule),
        )],
    ));
    for row in body_rows {
        for row_line in markdown_table_row_lines(&row, &widths) {
            lines.push(timeline_content_line(
                accent,
                vec![Span::styled(row_line, body_style)],
            ));
        }
    }
    lines.push(timeline_content_line(
        accent,
        vec![Span::styled(
            markdown_table_border(&widths, '└', '┴', '┘', '─'),
            Style::default().fg(palette.markdown_rule),
        )],
    ));
    lines
}

fn markdown_table_cells(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|cell| cell.trim().to_owned())
        .collect()
}

fn markdown_table_divider_cell(cell: &str) -> bool {
    !cell.is_empty()
        && cell
            .chars()
            .all(|character| matches!(character, '-' | ':' | ' '))
}

fn markdown_table_border(
    widths: &[usize],
    left: char,
    join: char,
    right: char,
    fill: char,
) -> String {
    let mut out = String::new();
    out.push(left);
    for (index, width) in widths.iter().enumerate() {
        out.push_str(&fill.to_string().repeat(width + 2));
        if index + 1 < widths.len() {
            out.push(join);
        }
    }
    out.push(right);
    out
}

fn markdown_table_row(cells: &[String], widths: &[usize]) -> String {
    let mut out = String::new();
    out.push('│');
    for (index, width) in widths.iter().enumerate() {
        let cell = cells.get(index).map(String::as_str).unwrap_or("");
        out.push(' ');
        out.push_str(cell);
        let cell_width = UnicodeWidthStr::width(cell);
        if *width > cell_width {
            out.push_str(&" ".repeat(*width - cell_width));
        }
        out.push(' ');
        out.push('│');
    }
    out
}

fn markdown_table_row_lines(cells: &[String], widths: &[usize]) -> Vec<String> {
    let wrapped_cells = widths
        .iter()
        .enumerate()
        .map(|(index, width)| {
            let cell = cells.get(index).map(String::as_str).unwrap_or("");
            wrap_display_width(cell, *width)
        })
        .collect::<Vec<_>>();
    let row_height = wrapped_cells.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let mut lines = Vec::with_capacity(row_height);
    for line_index in 0..row_height {
        let row = widths
            .iter()
            .enumerate()
            .map(|(column, width)| {
                let text = wrapped_cells[column]
                    .get(line_index)
                    .cloned()
                    .unwrap_or_default();
                pad_display_width(&text, *width)
            })
            .collect::<Vec<_>>();
        lines.push(markdown_table_row(&row, widths));
    }
    lines
}

fn clamp_table_widths(widths: &[usize], max_content_width: usize) -> Vec<usize> {
    if widths.is_empty() {
        return Vec::new();
    }
    let mut clamped = widths.to_vec();
    let min_widths = widths
        .iter()
        .map(|width| (*width).min(12).clamp(4, 12))
        .collect::<Vec<_>>();
    while markdown_table_total_width(&clamped) > max_content_width {
        let Some((index, _)) = clamped
            .iter()
            .enumerate()
            .filter(|(index, width)| **width > min_widths[*index])
            .max_by_key(|(_, width)| **width)
        else {
            break;
        };
        clamped[index] = clamped[index].saturating_sub(1);
    }
    clamped
}

fn markdown_table_total_width(widths: &[usize]) -> usize {
    if widths.is_empty() {
        return 0;
    }
    widths.iter().sum::<usize>() + widths.len() * 3 + 1
}

#[cfg(test)]
pub(crate) fn render_markdown_spans(
    line: &str,
    base_style: Style,
    state: &mut MarkdownRenderState,
    options: MarkdownRenderOptions,
) -> Vec<Span<'static>> {
    let palette = theme::default_palette();
    render_markdown_spans_with_palette(line, base_style, state, options, &palette)
}

pub(crate) fn render_markdown_spans_with_palette(
    line: &str,
    base_style: Style,
    state: &mut MarkdownRenderState,
    options: MarkdownRenderOptions,
    palette: &ThemePalette,
) -> Vec<Span<'static>> {
    if state.in_fenced_code {
        return render_code_line_spans_with_bg(
            line,
            palette.accent_info,
            Style::default().fg(palette.markdown_code_fg),
            palette.markdown_code_bg,
        );
    }
    if let Some((level, content)) = markdown_heading(line) {
        let accent = match level {
            1 => palette.markdown_heading,
            2 => palette.accent_info,
            3 => palette.accent_success,
            _ => palette.accent_secondary,
        };
        return render_inline_markdown_spans_with_palette(
            content,
            base_style.fg(accent).add_modifier(Modifier::BOLD),
            options,
            palette,
        );
    }
    if markdown_rule(line) {
        return vec![Span::styled(
            "────────────────────────────────",
            Style::default().fg(palette.markdown_rule),
        )];
    }
    if let Some((checked, content)) = markdown_task_item(line) {
        let marker = if checked { "[x]" } else { "[ ]" };
        let indent = markdown_list_indent(line);
        let mut spans = vec![Span::styled(
            format!("{}{marker} ", "  ".repeat(indent)),
            Style::default()
                .fg(if checked {
                    palette.accent_success
                } else {
                    palette.accent_warning
                })
                .add_modifier(Modifier::BOLD),
        )];
        spans.extend(render_inline_markdown_spans_with_palette(
            content, base_style, options, palette,
        ));
        return spans;
    }
    if let Some(content) = markdown_bullet_item(line) {
        let indent = markdown_list_indent(line);
        let mut spans = vec![Span::styled(
            format!("{}• ", "  ".repeat(indent)),
            Style::default()
                .fg(palette.accent_warning)
                .add_modifier(Modifier::BOLD),
        )];
        spans.extend(render_inline_markdown_spans_with_palette(
            content, base_style, options, palette,
        ));
        return spans;
    }
    if let Some((number, content)) = markdown_ordered_item(line) {
        let indent = markdown_list_indent(line);
        let mut spans = vec![Span::styled(
            format!("{}{number}. ", "  ".repeat(indent)),
            Style::default()
                .fg(palette.accent_warning)
                .add_modifier(Modifier::BOLD),
        )];
        spans.extend(render_inline_markdown_spans_with_palette(
            content, base_style, options, palette,
        ));
        return spans;
    }
    if let Some(content) = markdown_quote(line) {
        let mut spans = vec![Span::styled(
            "│ ",
            Style::default().fg(palette.markdown_quote_bar),
        )];
        spans.extend(render_inline_markdown_spans_with_palette(
            content,
            base_style.fg(palette.markdown_quote_text),
            options,
            palette,
        ));
        return spans;
    }
    if markdown_table_line(line) {
        return render_table_spans_with_palette(line, base_style, options, palette);
    }
    if line_looks_like_code(line) {
        return render_code_line_spans_with_bg(
            line,
            palette.accent_info,
            Style::default().fg(palette.markdown_code_fg),
            palette.markdown_code_bg,
        );
    }
    render_inline_markdown_spans_with_palette(line, base_style, options, palette)
}

#[allow(dead_code)]
pub(crate) fn render_inline_markdown_spans_with_options(
    text: &str,
    base_style: Style,
    options: MarkdownRenderOptions,
) -> Vec<Span<'static>> {
    let palette = theme::default_palette();
    render_inline_markdown_spans_with_palette(text, base_style, options, &palette)
}

pub(crate) fn render_inline_markdown_spans_with_palette(
    text: &str,
    base_style: Style,
    options: MarkdownRenderOptions,
    palette: &ThemePalette,
) -> Vec<Span<'static>> {
    let options = options.normalized();
    let mut spans = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let next_marker = next_inline_marker(rest);

        let Some(marker_index) = next_marker else {
            spans.push(Span::styled(rest.to_owned(), base_style));
            break;
        };

        if marker_index > 0 {
            spans.push(Span::styled(rest[..marker_index].to_owned(), base_style));
            rest = &rest[marker_index..];
            continue;
        }

        if let Some((label, url, consumed)) = markdown_link(rest) {
            spans.push(Span::styled(
                label.to_owned(),
                base_style
                    .fg(palette.markdown_link)
                    .add_modifier(Modifier::UNDERLINED | Modifier::BOLD),
            ));
            if options.show_link_urls {
                let url_width = options
                    .max_content_width
                    .saturating_sub(UnicodeWidthStr::width(label))
                    .saturating_sub(4)
                    .max(12);
                spans.push(Span::styled(
                    format!(" <{}>", truncate_display_width(url, url_width)),
                    Style::default().fg(palette.text_muted),
                ));
            }
            rest = &rest[consumed..];
            continue;
        }

        if let Some(after) = rest.strip_prefix("**") {
            if let Some(end) = after.find("**") {
                spans.push(Span::styled(
                    after[..end].to_owned(),
                    base_style.add_modifier(Modifier::BOLD),
                ));
                rest = &after[end + 2..];
            } else {
                spans.push(Span::styled("**".to_owned(), base_style));
                rest = after;
            }
            continue;
        }

        if let Some((content, consumed)) = markdown_emphasis(rest) {
            spans.push(Span::styled(
                content.to_owned(),
                base_style.add_modifier(Modifier::ITALIC),
            ));
            rest = &rest[consumed..];
            continue;
        }

        if let Some(after) = rest.strip_prefix('`') {
            if let Some(end) = after.find('`') {
                spans.push(Span::styled(
                    after[..end].to_owned(),
                    Style::default()
                        .fg(palette.markdown_code_fg)
                        .bg(palette.markdown_code_bg)
                        .add_modifier(Modifier::BOLD),
                ));
                rest = &after[end + 1..];
            } else {
                spans.push(Span::styled("`".to_owned(), base_style));
                rest = after;
            }
            continue;
        }

        spans.push(Span::styled(rest.to_owned(), base_style));
        break;
    }
    spans
}

pub(crate) fn markdown_plain_text(text: &str) -> String {
    let mut plain = String::new();
    let mut rest = text;
    while !rest.is_empty() {
        let next_marker = next_inline_marker(rest);
        let Some(marker_index) = next_marker else {
            plain.push_str(rest);
            break;
        };
        if marker_index > 0 {
            plain.push_str(&rest[..marker_index]);
            rest = &rest[marker_index..];
            continue;
        }
        if let Some((label, _, consumed)) = markdown_link(rest) {
            plain.push_str(label);
            rest = &rest[consumed..];
            continue;
        }
        if let Some(after) = rest.strip_prefix("**")
            && let Some(end) = after.find("**")
        {
            plain.push_str(&after[..end]);
            rest = &after[end + 2..];
            continue;
        }
        if let Some(after) = rest.strip_prefix('`')
            && let Some(end) = after.find('`')
        {
            plain.push_str(&after[..end]);
            rest = &after[end + 1..];
            continue;
        }
        if let Some((content, consumed)) = markdown_emphasis(rest) {
            plain.push_str(content);
            rest = &rest[consumed..];
            continue;
        }
        if let Some(character) = rest.chars().next() {
            plain.push(character);
            rest = &rest[character.len_utf8()..];
        } else {
            break;
        }
    }
    plain
}

#[allow(dead_code)]
pub(crate) fn render_code_line_spans(
    line: &str,
    accent: Color,
    base_style: Style,
) -> Vec<Span<'static>> {
    let palette = theme::default_palette();
    render_code_line_spans_with_bg(line, accent, base_style, palette.markdown_code_bg)
}

pub(crate) fn render_code_line_spans_with_bg(
    line: &str,
    accent: Color,
    base_style: Style,
    bg: Color,
) -> Vec<Span<'static>> {
    vec![
        Span::styled(
            "│ ",
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if line.is_empty() {
                " ".to_owned()
            } else {
                line.to_owned()
            },
            base_style.bg(bg),
        ),
    ]
}

fn render_highlighted_code_line_spans(
    spans: &[Span<'static>],
    accent: Color,
    base_style: Style,
    bg: Color,
) -> Vec<Span<'static>> {
    let mut rendered = vec![Span::styled(
        "│ ",
        Style::default().fg(accent).add_modifier(Modifier::BOLD),
    )];
    if spans.is_empty() {
        rendered.push(Span::styled(" ", base_style.bg(bg)));
        return rendered;
    }
    for span in spans {
        let style = base_style.bg(bg).patch(span.style);
        rendered.push(Span::styled(span.content.to_string(), style));
    }
    rendered
}

fn fenced_code_language(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    trimmed.strip_prefix("```").map(str::trim)
}

fn markdown_code_language_token(language: &str) -> &str {
    language.split_whitespace().next().unwrap_or(language)
}

fn markdown_heading(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let level = trimmed
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let remainder = trimmed[level..].trim_start();
    if remainder.is_empty() {
        None
    } else {
        Some((level, remainder))
    }
}

fn markdown_rule(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= 3
        && trimmed
            .chars()
            .all(|character| matches!(character, '-' | '*' | '_' | ' '))
}

fn markdown_task_item(line: &str) -> Option<(bool, &str)> {
    let trimmed = line.trim_start();
    let content = trimmed
        .strip_prefix("- [ ] ")
        .or_else(|| trimmed.strip_prefix("* [ ] "))
        .map(|content| (false, content));
    content.or_else(|| {
        trimmed
            .strip_prefix("- [x] ")
            .or_else(|| trimmed.strip_prefix("* [x] "))
            .map(|content| (true, content))
    })
}

fn markdown_bullet_item(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
}

fn markdown_ordered_item(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim_start();
    let digits = trimmed
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .count();
    if digits == 0 {
        return None;
    }
    let number = &trimmed[..digits];
    let rest = trimmed[digits..].strip_prefix(". ")?;
    Some((number, rest))
}

fn markdown_list_indent(line: &str) -> usize {
    line.chars()
        .take_while(|character| matches!(character, ' ' | '\t'))
        .map(|character| if character == '\t' { 4 } else { 1 })
        .sum::<usize>()
        / 2
}

fn markdown_quote(line: &str) -> Option<&str> {
    line.trim_start().strip_prefix("> ")
}

fn markdown_table_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('|') && trimmed.ends_with('|') && trimmed.matches('|').count() >= 2
}

fn markdown_link(text: &str) -> Option<(&str, &str, usize)> {
    let after_label = text.strip_prefix('[')?;
    let label_end = after_label.find("](")?;
    let label = &after_label[..label_end];
    let after_url = &after_label[label_end + 2..];
    let url_end = after_url.find(')')?;
    let url = &after_url[..url_end];
    let consumed = 1 + label_end + 2 + url_end + 1;
    Some((label, url, consumed))
}

fn markdown_emphasis(text: &str) -> Option<(&str, usize)> {
    for marker in ['*', '_'] {
        let Some(after) = text.strip_prefix(marker) else {
            continue;
        };
        if after.starts_with(marker) {
            continue;
        }
        let end = emphasis_end(after, marker)?;
        let content = &after[..end];
        if content.is_empty() {
            continue;
        }
        return Some((content, 1 + end + 1));
    }
    None
}

fn emphasis_end(text: &str, marker: char) -> Option<usize> {
    if marker != '_' {
        return text.find(marker);
    }
    for (index, character) in text.char_indices() {
        if character != marker {
            continue;
        }
        let previous = text[..index].chars().next_back();
        let next = text[index + marker.len_utf8()..].chars().next();
        if previous.is_some_and(char::is_alphanumeric) && next.is_some_and(char::is_alphanumeric) {
            continue;
        }
        return Some(index);
    }
    None
}

fn next_inline_marker(text: &str) -> Option<usize> {
    let markers = [
        text.find("**"),
        text.find('`'),
        text.find('['),
        text.find('*'),
        next_underscore_marker(text),
    ];
    markers.into_iter().flatten().min()
}

fn next_underscore_marker(text: &str) -> Option<usize> {
    for (index, character) in text.char_indices() {
        if character != '_' {
            continue;
        }
        let previous = text[..index].chars().next_back();
        let next = text[index + character.len_utf8()..].chars().next();
        if previous.is_some_and(char::is_alphanumeric) && next.is_some_and(char::is_alphanumeric) {
            continue;
        }
        return Some(index);
    }
    None
}

#[allow(dead_code)]
fn render_table_spans(
    line: &str,
    base_style: Style,
    options: MarkdownRenderOptions,
) -> Vec<Span<'static>> {
    let palette = theme::default_palette();
    render_table_spans_with_palette(line, base_style, options, &palette)
}

fn render_table_spans_with_palette(
    line: &str,
    base_style: Style,
    options: MarkdownRenderOptions,
    palette: &ThemePalette,
) -> Vec<Span<'static>> {
    let cells = line
        .trim()
        .trim_matches('|')
        .split('|')
        .map(str::trim)
        .collect::<Vec<_>>();
    if cells.is_empty() {
        return vec![Span::styled(line.to_owned(), base_style)];
    }
    if cells.iter().all(|cell| {
        !cell.is_empty()
            && cell
                .chars()
                .all(|character| matches!(character, '-' | ':' | ' '))
    }) {
        let width = cells
            .iter()
            .map(|cell| cell.len().max(3) + 2)
            .sum::<usize>()
            .saturating_add(cells.len().saturating_sub(1) * 3)
            .max(12);
        return vec![Span::styled(
            "┄".repeat(width),
            Style::default().fg(palette.markdown_rule),
        )];
    }
    let mut spans = vec![Span::styled(
        "│ ",
        Style::default().fg(palette.markdown_rule),
    )];
    for (index, cell) in cells.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(
                " │ ",
                Style::default().fg(palette.markdown_rule),
            ));
        }
        if cell.is_empty() {
            spans.push(Span::styled(" ", base_style));
        } else {
            spans.extend(render_inline_markdown_spans_with_palette(
                cell, base_style, options, palette,
            ));
        }
    }
    spans.push(Span::styled(
        " │",
        Style::default().fg(palette.markdown_rule),
    ));
    spans
}

fn line_looks_like_code(line: &str) -> bool {
    let trimmed = line.trim_start();
    line.starts_with("    ")
        || line.starts_with('\t')
        || line.contains('│')
        || line.contains('└')
        || line.contains('├')
        || line.contains('┌')
        || line.contains('─')
        || trimmed.starts_with('{')
        || trimmed.starts_with('}')
        || trimmed.starts_with('[')
        || trimmed.starts_with(']')
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/markdown_tests.rs"]
mod tests;
