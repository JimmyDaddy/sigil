use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use super::super::{
    primitives::{timeline_content_line, timeline_section_line_with_palette},
    text::{truncate_display_width, wrap_display_width},
    theme::ThemePalette,
};

const MAX_SOURCE_BYTES: usize = 32 * 1024;
const MAX_SOURCE_LINES: usize = 1_000;
pub(super) const MAX_DIAGRAMS_PER_MESSAGE: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum MermaidAdmission {
    Ready { diagram_type: String },
    Generating,
    Oversize,
    TooMany,
    Rejected,
}

pub(super) fn admit_mermaid(source: &str, closed: bool) -> MermaidAdmission {
    if !closed {
        return MermaidAdmission::Generating;
    }
    if source.len() > MAX_SOURCE_BYTES || source.lines().count() > MAX_SOURCE_LINES {
        return MermaidAdmission::Oversize;
    }
    if source.chars().any(|character| {
        character == '\0' || (character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    }) || source.lines().any(|line| {
        let trimmed = line.trim_start();
        let normalized = trimmed.to_ascii_lowercase();
        trimmed.starts_with("%%{")
            || normalized.starts_with("click ")
            || normalized.contains("http:")
            || normalized.contains("https:")
            || normalized.contains("file:")
            || normalized.contains("data:")
            || normalized.contains("javascript:")
            || normalized.contains("foreignobject")
            || normalized.contains("image:")
            || contains_html_tag(trimmed)
    }) {
        return MermaidAdmission::Rejected;
    }
    MermaidAdmission::Ready {
        diagram_type: diagram_type(source),
    }
}

fn contains_html_tag(line: &str) -> bool {
    line.as_bytes()
        .windows(2)
        .any(|pair| pair[0] == b'<' && (pair[1] == b'/' || pair[1].is_ascii_alphabetic()))
}

pub(super) fn render_mermaid_section(
    accent: ratatui::style::Color,
    source_lines: &[&str],
    closed: bool,
    within_message_limit: bool,
    show_source: bool,
    max_content_width: usize,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let source = source_lines.join("\n");
    let admission = if within_message_limit {
        admit_mermaid(&source, closed)
    } else {
        MermaidAdmission::TooMany
    };
    let (diagram_type, status) = match &admission {
        MermaidAdmission::Ready { diagram_type } => (diagram_type.as_str(), "ready"),
        MermaidAdmission::Generating => ("mermaid", "generating"),
        MermaidAdmission::Oversize => ("mermaid", "too large"),
        MermaidAdmission::TooMany => ("mermaid", "message limit"),
        MermaidAdmission::Rejected => ("mermaid", "source only"),
    };
    let detail = format!("mermaid · {diagram_type} · {status}");
    let mut lines = vec![timeline_section_line_with_palette(
        accent,
        "diagram",
        palette.markdown_diagram,
        Vec::new(),
        palette,
    )];
    for row in wrap_display_width(&detail, max_content_width.saturating_sub(2).max(1)) {
        lines.push(timeline_content_line(
            accent,
            vec![Span::styled(
                row,
                Style::default()
                    .fg(palette.markdown_diagram)
                    .add_modifier(Modifier::BOLD),
            )],
        ));
    }

    let source_nonempty = source
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    lines.push(timeline_content_line(
        accent,
        vec![
            Span::styled(
                if show_source {
                    "source · "
                } else {
                    "summary · "
                },
                Style::default()
                    .fg(palette.text_muted)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                truncate_display_width(
                    source_nonempty,
                    max_content_width.saturating_sub(14).max(8),
                ),
                Style::default().fg(palette.text_secondary),
            ),
        ],
    ));

    if show_source || matches!(admission, MermaidAdmission::TooMany) {
        for source_line in source.split('\n') {
            for row in wrap_display_width(source_line, max_content_width.saturating_sub(4).max(1)) {
                lines.push(timeline_content_line(
                    accent,
                    vec![
                        Span::styled(
                            "│ ",
                            Style::default()
                                .fg(palette.markdown_diagram)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(row, Style::default().fg(palette.markdown_code_fg)),
                    ],
                ));
            }
        }
    }
    lines
}

fn diagram_type(source: &str) -> String {
    source
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("%%"))
        .and_then(|line| line.split_whitespace().next())
        .filter(|token| {
            matches!(
                token.to_ascii_lowercase().as_str(),
                "flowchart"
                    | "graph"
                    | "sequencediagram"
                    | "classdiagram"
                    | "statediagram"
                    | "statediagram-v2"
                    | "erdiagram"
                    | "journey"
                    | "gantt"
                    | "pie"
                    | "mindmap"
                    | "timeline"
                    | "quadrantchart"
                    | "xychart-beta"
                    | "architecture-beta"
            )
        })
        .unwrap_or("mermaid")
        .to_owned()
}
