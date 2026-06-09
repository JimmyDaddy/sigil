use ratatui::style::{Color, Modifier, Style};

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

pub(crate) fn diff_line_number_gutter(old_line: Option<usize>, new_line: Option<usize>) -> String {
    let old = old_line
        .map(|line| format!("{line:>4}"))
        .unwrap_or_else(|| "    ".to_owned());
    let new = new_line
        .map(|line| format!("{line:>4}"))
        .unwrap_or_else(|| "    ".to_owned());
    format!("{old} {new} │ ")
}

pub(crate) fn diff_line_style(kind: DiffLineKind) -> (Color, Style) {
    match kind {
        DiffLineKind::Header => (
            Color::Blue,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        DiffLineKind::Hunk => (
            Color::Yellow,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        DiffLineKind::Added => (
            Color::Green,
            Style::default().fg(Color::Green).bg(Color::Rgb(16, 34, 22)),
        ),
        DiffLineKind::Removed => (
            Color::Red,
            Style::default().fg(Color::Red).bg(Color::Rgb(40, 18, 18)),
        ),
        DiffLineKind::Context => (Color::DarkGray, Style::default().fg(Color::Gray)),
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

fn is_no_newline_marker(line: &str) -> bool {
    line.starts_with("\\ No newline at end of file")
}

#[cfg(test)]
mod tests {
    use super::{diff_line_number_gutter, number_unified_diff_lines};

    #[test]
    fn number_unified_diff_lines_tracks_old_and_new_columns() {
        let lines = [
            "--- current/note.txt",
            "+++ proposed/note.txt",
            "@@ -2,3 +2,4 @@",
            " context",
            "-old",
            "+new",
            " tail",
        ];

        let numbered = number_unified_diff_lines(lines);

        assert_eq!(numbered[0].old_line, None);
        assert_eq!(numbered[0].new_line, None);
        assert_eq!(numbered[2].old_line, None);
        assert_eq!(numbered[2].new_line, None);
        assert_eq!(numbered[3].old_line, Some(2));
        assert_eq!(numbered[3].new_line, Some(2));
        assert_eq!(numbered[4].old_line, Some(3));
        assert_eq!(numbered[4].new_line, None);
        assert_eq!(numbered[5].old_line, None);
        assert_eq!(numbered[5].new_line, Some(3));
        assert_eq!(numbered[6].old_line, Some(4));
        assert_eq!(numbered[6].new_line, Some(4));
    }

    #[test]
    fn diff_line_number_gutter_uses_stable_columns() {
        assert_eq!(diff_line_number_gutter(Some(12), None), "  12      │ ");
        assert_eq!(diff_line_number_gutter(None, Some(3)), "        3 │ ");
        assert_eq!(diff_line_number_gutter(Some(4), Some(5)), "   4    5 │ ");
    }
}
