use ratatui::style::{Color, Modifier, Style};

use super::theme::{self, ThemePalette};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiffLineKind {
    Header,
    Hunk,
    Added,
    Removed,
    Context,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NumberedDiffLine<'a> {
    pub(crate) text: &'a str,
    pub(crate) kind: DiffLineKind,
    pub(crate) old_line: Option<usize>,
    pub(crate) new_line: Option<usize>,
}

pub(crate) fn diff_line_kind(line: &str) -> DiffLineKind {
    if line.starts_with("---")
        || line.starts_with("+++")
        || line.starts_with("diff ")
        || line.starts_with("index ")
    {
        DiffLineKind::Header
    } else if line.starts_with("@@") {
        DiffLineKind::Hunk
    } else if line.starts_with('+') && !line.starts_with("+++") {
        DiffLineKind::Added
    } else if line.starts_with('-') && !line.starts_with("---") {
        DiffLineKind::Removed
    } else {
        DiffLineKind::Context
    }
}

pub(crate) fn number_unified_diff_lines<'a>(
    lines: impl IntoIterator<Item = &'a str>,
) -> Vec<NumberedDiffLine<'a>> {
    let mut old_next = None::<usize>;
    let mut new_next = None::<usize>;
    let mut in_hunk = false;
    let mut numbered = Vec::new();

    for line in lines {
        let kind = diff_line_kind(line);
        let mut old_line = None;
        let mut new_line = None;
        match kind {
            DiffLineKind::Hunk => {
                if let Some((old_start, new_start)) = parse_hunk_starts(line) {
                    old_next = Some(old_start);
                    new_next = Some(new_start);
                    in_hunk = true;
                } else {
                    old_next = None;
                    new_next = None;
                    in_hunk = false;
                }
            }
            DiffLineKind::Header => {
                old_next = None;
                new_next = None;
                in_hunk = false;
            }
            DiffLineKind::Added if in_hunk => {
                new_line = new_next;
                if let Some(next) = new_next.as_mut() {
                    *next += 1;
                }
            }
            DiffLineKind::Removed if in_hunk => {
                old_line = old_next;
                if let Some(next) = old_next.as_mut() {
                    *next += 1;
                }
            }
            DiffLineKind::Context if in_hunk && !is_no_newline_marker(line) => {
                old_line = old_next;
                new_line = new_next;
                if let Some(next) = old_next.as_mut() {
                    *next += 1;
                }
                if let Some(next) = new_next.as_mut() {
                    *next += 1;
                }
            }
            DiffLineKind::Context | DiffLineKind::Added | DiffLineKind::Removed => {}
        }
        numbered.push(NumberedDiffLine {
            text: line,
            kind,
            old_line,
            new_line,
        });
    }

    numbered
}

#[cfg(test)]
fn diff_line_number_gutter(old_line: Option<usize>, new_line: Option<usize>) -> String {
    let width = [old_line, new_line]
        .into_iter()
        .flatten()
        .map(line_number_digits)
        .max()
        .unwrap_or(2)
        .max(2);
    format!(
        "{} {}│ ",
        diff_line_number_text(old_line, width),
        diff_line_number_text(new_line, width)
    )
}

pub(crate) fn diff_line_number_width(lines: &[NumberedDiffLine<'_>]) -> usize {
    lines
        .iter()
        .flat_map(|line| [line.old_line, line.new_line])
        .flatten()
        .map(line_number_digits)
        .max()
        .unwrap_or(2)
        .max(2)
}

pub(crate) fn diff_line_number_text(line: Option<usize>, width: usize) -> String {
    line.map(|line| format!("{line:>width$}"))
        .unwrap_or_else(|| " ".repeat(width))
}

pub(crate) fn diff_line_style(kind: DiffLineKind) -> (Color, Style) {
    let palette = theme::default_palette();
    diff_line_style_for_palette(kind, &palette)
}

pub(crate) fn diff_line_style_for_palette(
    kind: DiffLineKind,
    palette: &ThemePalette,
) -> (Color, Style) {
    match kind {
        DiffLineKind::Header => (
            palette.diff_header_fg,
            Style::default()
                .fg(palette.diff_header_fg)
                .add_modifier(Modifier::BOLD),
        ),
        DiffLineKind::Hunk => (
            palette.diff_hunk_fg,
            Style::default()
                .fg(palette.diff_hunk_fg)
                .add_modifier(Modifier::BOLD),
        ),
        DiffLineKind::Added => (
            palette.diff_added_fg,
            Style::default()
                .fg(palette.diff_added_fg)
                .bg(palette.diff_added_bg),
        ),
        DiffLineKind::Removed => (
            palette.diff_removed_fg,
            Style::default()
                .fg(palette.diff_removed_fg)
                .bg(palette.diff_removed_bg),
        ),
        DiffLineKind::Context => (
            palette.diff_gutter_fg,
            Style::default().fg(palette.diff_context_fg),
        ),
    }
}

fn parse_hunk_starts(line: &str) -> Option<(usize, usize)> {
    let mut parts = line.split_whitespace();
    (parts.next()? == "@@").then_some(())?;
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    Some((parse_range_start(old)?, parse_range_start(new)?))
}

fn parse_range_start(range: &str) -> Option<usize> {
    range.split(',').next()?.parse().ok()
}

fn line_number_digits(line: usize) -> usize {
    line.to_string().len()
}

fn is_no_newline_marker(line: &str) -> bool {
    line.starts_with("\\ No newline at end of file")
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/diff_tests.rs"]
mod tests;
