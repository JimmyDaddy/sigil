use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use serde_json::Value;
use sigil_kernel::SyntaxThemeId;
use unicode_width::UnicodeWidthStr;

use crate::{
    agent_display::{AgentDisplayNameInput, resolve_agent_display_name},
    app::TimelineEntry,
};

use super::{
    TimelineRenderOptions,
    diff::{
        DiffLineKind, NumberedDiffLine, diff_line_kind, diff_line_number_text,
        diff_line_number_width, diff_line_style_for_palette, number_unified_diff_lines,
    },
    markdown::{
        MarkdownRenderOptions, render_code_line_spans_with_bg,
        render_markdown_timeline_lines_with_palette,
    },
    primitives::{
        section_badge_with_palette, timeline_badge_with_palette, timeline_content_line,
        timeline_section_line_with_palette,
    },
    status_indicator::{StatusIndicator, StatusKind, status_kind_from_label},
    syntax_highlight::highlight_code_to_spans_with_theme,
    text::truncate_inline_text,
    theme::ThemePalette,
};

const COLLAPSED_TOOL_PREVIEW_VISIBLE_ROWS: usize = 4;

mod display;
mod frame;
mod parser;
mod preview;

use display::*;
use frame::*;
use parser::*;
use preview::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolActivityView {
    pub(crate) key: String,
    pub(crate) title: String,
    pub(crate) is_inspection: bool,
    pub(crate) defaults_expanded: bool,
}

pub(crate) fn tool_activity_view(
    entry: &TimelineEntry,
    _entry_index: usize,
) -> Option<ToolActivityView> {
    let summary = parse_tool_summary(&entry.text);
    Some(build_tool_activity_view(&summary, &entry.text))
}

pub(crate) fn render_tool_entry_lines(
    entry: &TimelineEntry,
    options: &TimelineRenderOptions,
    _entry_index: usize,
) -> Vec<Line<'static>> {
    let summary = parse_tool_summary(&entry.text);
    let display = build_tool_card_display(&summary);
    let activity = build_tool_activity_view(&summary, &entry.text);
    let palette = &options.theme.palette;
    let accent = palette.accent_danger;
    let selected = options
        .selected_tool_activity_key
        .as_deref()
        .is_some_and(|selected| selected == activity.key.as_str());
    let hovered = options
        .hovered_tool_activity_key
        .as_deref()
        .is_some_and(|hovered| hovered == activity.key.as_str());
    let activity_marker_style =
        tool_card_activity_marker_style(display.status.kind, hovered, palette);
    let default_expanded = activity.defaults_expanded;
    let expanded = options.expand_tool_previews
        || options.expanded_tool_activity_keys.contains(&activity.key)
        || (default_expanded && !options.collapsed_tool_activity_keys.contains(&activity.key));
    let mut lines = vec![tool_card_header_line(
        &display,
        activity_marker_style,
        expanded,
        options.max_content_width,
        palette,
    )];
    if let Some(summary_line) = display.summary.clone() {
        lines.push(timeline_content_line(
            accent,
            vec![Span::styled(
                summary_line,
                Style::default()
                    .fg(palette.text_muted)
                    .add_modifier(Modifier::ITALIC),
            )],
        ));
    }
    if tool_has_preview(&summary) {
        if expanded {
            let body = render_tool_preview_body_with_palette(
                &summary,
                accent,
                options.max_content_width,
                options.theme.syntax_theme,
                palette,
            );
            lines.extend(limit_expanded_tool_preview_body(
                &summary,
                body,
                options
                    .tool_activity_visible_rows
                    .get(&activity.key)
                    .copied(),
                accent,
                palette,
            ));
        } else {
            lines.extend(render_tool_collapsed_preview_body_with_palette(
                &summary,
                accent,
                options.max_content_width,
                options.theme.syntax_theme,
                palette,
            ));
        }
    }
    tool_card_frame_lines(
        lines,
        selected,
        options.max_content_width,
        activity_marker_style,
        palette,
    )
}

fn limit_expanded_tool_preview_body(
    summary: &ToolCardRender,
    body: Vec<Line<'static>>,
    visible_rows: Option<usize>,
    accent: Color,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let Some(visible_rows) = visible_rows else {
        return body;
    };
    if body.len() <= visible_rows {
        return body;
    }
    let visible_rows = visible_rows.max(COLLAPSED_TOOL_PREVIEW_VISIBLE_ROWS);
    let hidden_rows = collapsed_tool_hidden_rows(summary, body.len(), visible_rows);
    let mut lines = body.into_iter().take(visible_rows).collect::<Vec<_>>();
    lines.extend(render_tool_hidden_tail(accent, hidden_rows, palette));
    lines
}

fn tool_name_matches(tool_name: &str, expected: &str) -> bool {
    tool_name == expected || tool_name.ends_with(&format!("_{expected}"))
}

#[derive(Clone)]
struct ToolCardRender {
    call_id: Option<String>,
    tool_name: String,
    is_error: bool,
    error_kind: Option<String>,
    summary: Option<String>,
    metadata: ToolCardMetadata,
    preview_kind: ToolPreviewKind,
    preview_language: Option<String>,
    preview_lines: Vec<String>,
    hidden_lines: usize,
    preview_value: Option<Value>,
    diff: Option<ToolCardDiff>,
}

#[derive(Clone, Default)]
struct ToolCardMetadata {
    exit_code: Option<i64>,
    stdout_bytes: Option<u64>,
    stderr_bytes: Option<u64>,
    changed_files: Vec<String>,
    call_summary: Option<String>,
    action: Option<String>,
    mcp_server: Option<String>,
    mcp_tool: Option<String>,
    mcp_trust_class: Option<String>,
    code_server: Option<String>,
    code_capability: Option<String>,
    returned_entries: Option<u64>,
    total_entries: Option<u64>,
    execution_backend: Option<String>,
    execution_network_policy: Option<String>,
    execution_timeout_source: Option<String>,
    execution_cleanup_status: Option<String>,
    shell_command_family: Option<String>,
    shell_verdict: Option<String>,
    terminal_cleanup_status: Option<String>,
    terminal_task_id: Option<String>,
    terminal_status: Option<String>,
    terminal_command: Option<String>,
    terminal_log_path: Option<String>,
    terminal_exit_code: Option<i64>,
    terminal_failed_reason: Option<String>,
}

struct ToolCardDisplay {
    title: ToolCardTitle,
    status: ToolCardDisplayStatus,
    summary: Option<String>,
}

struct ToolCardTitle {
    action: String,
    subject: String,
    args: Option<String>,
}

impl ToolCardTitle {
    fn new(action: impl Into<String>, subject: impl Into<String>, args: Option<String>) -> Self {
        Self {
            action: action.into(),
            subject: subject.into(),
            args,
        }
    }

    fn plain(&self) -> String {
        match &self.args {
            Some(args) if !args.is_empty() => {
                format!("{} {} {}", self.action, self.subject, args)
            }
            _ => format!("{} {}", self.action, self.subject),
        }
    }
}

struct ToolCardDisplayStatus {
    label: &'static str,
    detail: Option<String>,
    kind: StatusKind,
    is_error: bool,
}

struct ShellSearch {
    pattern: String,
    location: Option<String>,
}

fn classify_simple_shell_search(command: &str) -> Option<ShellSearch> {
    if command_contains_shell_control(command) {
        return None;
    }
    let tokens = simple_shell_tokens(command)?;
    let (program, args) = tokens.split_first()?;
    if program.contains('=') {
        return None;
    }
    let program = program.rsplit('/').next().unwrap_or(program.as_str());
    match program {
        "rg" => classify_pattern_search_args(args, &["-e", "--regexp", "-g", "--glob"]),
        "grep" => classify_pattern_search_args(args, &["-e", "--regexp", "-f", "--file"]),
        "fd" => classify_pattern_search_args(args, &["-e", "--extension", "-t", "--type"]),
        "find" => classify_find_search_args(args),
        _ => None,
    }
}

fn command_contains_shell_control(command: &str) -> bool {
    command
        .chars()
        .any(|character| matches!(character, '|' | '>' | '<' | ';' | '&' | '`' | '$'))
}

fn simple_shell_tokens(command: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for character in command.trim().chars() {
        match quote {
            Some(active) if character == active => quote = None,
            Some(_) => current.push(character),
            None if character == '\'' || character == '"' => quote = Some(character),
            None if character.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            None => current.push(character),
        }
    }
    if quote.is_some() {
        return None;
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    (!tokens.is_empty()).then_some(tokens)
}

fn classify_pattern_search_args(
    args: &[String],
    options_with_values: &[&str],
) -> Option<ShellSearch> {
    let mut positional = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let token = &args[index];
        if token == "--" {
            positional.extend(args[index + 1..].iter().cloned());
            break;
        }
        if options_with_values.iter().any(|option| token == option) {
            index += 2;
            continue;
        }
        if token.starts_with("--") && token.contains('=') {
            index += 1;
            continue;
        }
        if token.starts_with('-') {
            index += 1;
            continue;
        }
        positional.push(token.clone());
        index += 1;
    }
    let pattern = positional.first()?.clone();
    Some(ShellSearch {
        pattern,
        location: positional.get(1).cloned(),
    })
}

fn classify_find_search_args(args: &[String]) -> Option<ShellSearch> {
    let location = args
        .iter()
        .find(|token| !token.starts_with('-') && token.as_str() != ".")
        .cloned()
        .or_else(|| args.first().cloned());
    let pattern = args
        .windows(2)
        .find_map(|window| {
            matches!(
                window[0].as_str(),
                "-name" | "-iname" | "-path" | "-ipath" | "-regex"
            )
            .then(|| window[1].clone())
        })
        .or_else(|| {
            args.iter()
                .find(|token| token.contains('*') || token.contains('?'))
                .cloned()
        })?;
    Some(ShellSearch { pattern, location })
}

#[derive(Clone)]
struct ToolCardDiff {
    summary: String,
    truncated: bool,
    original_line_count: usize,
    rendered_line_count: usize,
    files: Vec<ToolCardDiffFile>,
}

#[derive(Clone)]
struct ToolCardDiffFile {
    path: String,
    lines: Vec<String>,
    truncated: bool,
    original_line_count: usize,
    rendered_line_count: usize,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ToolPreviewKind {
    Markdown,
    Json,
    Code,
    #[default]
    Text,
}

impl ToolPreviewKind {
    fn label(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Json => "json",
            Self::Code => "code",
            Self::Text => "text",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Markdown => "formatted preview",
            Self::Json => "structured preview",
            Self::Code => "code excerpt",
            Self::Text => "captured output",
        }
    }

    fn from_value(value: &str) -> Self {
        match value {
            "markdown" => Self::Markdown,
            "json" => Self::Json,
            "code" => Self::Code,
            _ => Self::Text,
        }
    }
}

fn tool_status_style(kind: StatusKind, palette: &ThemePalette) -> Style {
    StatusIndicator::static_kind(kind).badge_style_with_palette(palette)
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/tool_card_tests.rs"]
mod tests;
