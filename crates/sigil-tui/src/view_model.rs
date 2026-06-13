use std::{env, path::Path};

use ratatui::text::Line;

use crate::{
    app::{AppState, PaneFocus},
    commands::{global_control_hints, tool_card_control_hints},
    timeline::RunPhase,
};

#[derive(Debug, Clone)]
pub(crate) struct UiViewModel {
    pub info_rail: InfoRailViewModel,
    pub composer: ComposerViewModel,
    pub footer: FooterViewModel,
}

impl UiViewModel {
    pub(crate) fn from_app(app: &AppState) -> Self {
        Self {
            info_rail: InfoRailViewModel::from_app(app),
            composer: ComposerViewModel::from_app(app),
            footer: FooterViewModel::from_app(app),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct InfoRailViewModel {
    pub session_title: String,
    pub workspace_label: String,
    pub session_lines: Vec<String>,
    pub permission_lines: Vec<String>,
    pub agent_lines: Vec<String>,
    pub mcp_lines: Vec<String>,
    pub code_lines: Vec<String>,
    pub usage_lines: Vec<String>,
    pub controls: Vec<String>,
}

impl InfoRailViewModel {
    fn from_app(app: &AppState) -> Self {
        let mut controls = global_control_hints(app.is_busy && app.pending_approval.is_none());
        if app.has_tool_cards() {
            controls.retain(|hint| !hint.starts_with("Ctrl-T: thinking"));
            controls.extend(tool_card_control_hints());
        }

        Self {
            session_title: app.session_display_title(),
            workspace_label: display_path_label(&app.workspace_root),
            session_lines: app
                .session_sidebar_lines()
                .into_iter()
                .chain(std::iter::once(if app.memory_enabled {
                    format!(
                        "memory: {} docs · {}",
                        app.memory_document_count, app.memory_last_status
                    )
                } else {
                    "memory: off".to_owned()
                }))
                .collect(),
            permission_lines: app.permission_card_lines(),
            agent_lines: app
                .agent_sidebar_rows()
                .into_iter()
                .map(|row| {
                    format!(
                        "{} {}: {}",
                        if row.selected {
                            ">"
                        } else if row.muted {
                            "~"
                        } else {
                            "-"
                        },
                        row.label,
                        row.detail
                    )
                })
                .collect(),
            mcp_lines: app.mcp_sidebar_lines(),
            code_lines: app.code_intelligence_sidebar_lines(),
            usage_lines: app.usage_sidebar_lines().to_vec(),
            controls,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ComposerViewModel {
    pub mode_label: String,
    pub phase: RunPhase,
    pub provider_name: String,
    pub model_name: String,
    pub reasoning_effort_label: String,
    pub input: String,
    pub input_rows: u16,
    pub cursor_position: (u16, u16),
}

impl ComposerViewModel {
    fn from_app(app: &AppState) -> Self {
        Self {
            mode_label: "Build".to_owned(),
            phase: app.run_phase(),
            provider_name: app.provider_name.clone(),
            model_name: app.model_name.clone(),
            reasoning_effort_label: app.reasoning_effort_label().to_owned(),
            input: app.input.clone(),
            input_rows: app.composer_input_rows(),
            cursor_position: app.input_cursor_visual_position(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FooterViewModel {
    pub phase: RunPhase,
    pub is_busy: bool,
    pub run_label: String,
    pub hints: String,
    pub context_label: String,
}

impl FooterViewModel {
    fn from_app(app: &AppState) -> Self {
        Self {
            phase: app.run_phase(),
            is_busy: app.is_busy && app.pending_approval.is_none(),
            run_label: footer_run_label(app),
            hints: footer_hints(app),
            context_label: app.context_usage_line(),
        }
    }
}

fn footer_run_label(app: &AppState) -> String {
    if let Some(activity) = app.live_activity_summary() {
        return format!("{} · {}", activity.label, activity.detail);
    }
    "ready".to_owned()
}

fn footer_hints(app: &AppState) -> String {
    if app.pending_approval.is_some() {
        return "Y allow · N deny · V diff".to_owned();
    }
    if app.is_busy {
        return "Esc interrupt · Ctrl-T details".to_owned();
    }
    if app.active_pane == PaneFocus::Composer && app.has_slash_selector() {
        return "↑↓ choose · Tab accept · Enter run · Esc close".to_owned();
    }
    "Enter send · Shift-Enter newline · / commands".to_owned()
}

#[derive(Debug, Clone)]
pub(crate) struct LivePanelViewModel {
    pub phase: RunPhase,
    pub progress: Option<LiveProgressViewModel>,
    pub transcript_lines: Vec<Line<'static>>,
}

impl LivePanelViewModel {
    pub(crate) fn from_app(app: &AppState, transcript_rows: usize) -> Self {
        Self {
            phase: app.run_phase(),
            progress: app
                .live_activity_summary()
                .map(|summary| LiveProgressViewModel::from_parts(&summary.label, &summary.detail)),
            transcript_lines: app.transcript_lines(transcript_rows),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LiveProgressViewModel {
    pub title: String,
    pub detail: String,
}

impl LiveProgressViewModel {
    fn from_parts(label: &str, detail: &str) -> Self {
        let title = match label {
            "thinking" => "Thinking".to_owned(),
            "tool" => tool_progress_title(detail),
            "mcp" => "MCP".to_owned(),
            "streaming" => "Replying".to_owned(),
            "approval" => "Approval".to_owned(),
            _ => "Working".to_owned(),
        };
        Self {
            title,
            detail: detail.to_owned(),
        }
    }
}

fn tool_progress_title(detail: &str) -> String {
    let tool_name = detail
        .strip_prefix("running ")
        .unwrap_or(detail)
        .split_whitespace()
        .next()
        .unwrap_or("tool");
    match tool_name {
        "bash" => "Bash".to_owned(),
        "read_file" => "Read".to_owned(),
        "write_file" => "Write".to_owned(),
        "edit_file" => "Edit".to_owned(),
        "delete_file" => "Delete".to_owned(),
        "grep" => "Search".to_owned(),
        "glob" | "ls" => "Inspect".to_owned(),
        other => title_case_tool_name(other),
    }
}

fn title_case_tool_name(tool_name: &str) -> String {
    let words = tool_name
        .split(['_', '-'])
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    if words.is_empty() {
        return "Tool".to_owned();
    }
    words
        .into_iter()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn display_path_label(path: &Path) -> String {
    let display = path.to_string_lossy().into_owned();
    if let Ok(home) = env::var("HOME")
        && let Some(suffix) = display.strip_prefix(&home)
    {
        return format!("~{suffix}");
    }
    display
}

#[cfg(test)]
#[path = "tests/view_model_tests.rs"]
mod tests;
