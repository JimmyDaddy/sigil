use std::{env, path::Path};

use ratatui::text::Line;

use crate::{
    app::AppState,
    commands::{global_control_hints, tool_card_control_hints},
    timeline::RunPhase,
};

#[derive(Debug, Clone)]
pub(crate) struct UiViewModel {
    pub info_rail: InfoRailViewModel,
    pub composer: ComposerViewModel,
}

impl UiViewModel {
    pub(crate) fn from_app(app: &AppState) -> Self {
        Self {
            info_rail: InfoRailViewModel::from_app(app),
            composer: ComposerViewModel::from_app(app),
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
    pub usage_lines: Vec<String>,
    pub controls: Vec<String>,
}

impl InfoRailViewModel {
    fn from_app(app: &AppState) -> Self {
        let mut controls = global_control_hints(app.is_busy);
        if app.has_tool_cards() {
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
                        if row.selected { ">" } else { "-" },
                        row.label,
                        row.detail
                    )
                })
                .collect(),
            usage_lines: app.usage_sidebar_lines().to_vec(),
            controls,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ComposerViewModel {
    pub phase: RunPhase,
    pub is_busy: bool,
    pub model_name: String,
    pub reasoning_effort_label: String,
    pub run_phase_label: String,
    pub input: String,
    pub input_rows: u16,
    pub cursor_position: (u16, u16),
}

impl ComposerViewModel {
    fn from_app(app: &AppState) -> Self {
        Self {
            phase: app.run_phase(),
            is_busy: app.is_busy,
            model_name: app.model_name.clone(),
            reasoning_effort_label: app.reasoning_effort_label().to_owned(),
            run_phase_label: app.run_phase_label(),
            input: app.input.clone(),
            input_rows: app.composer_input_rows(),
            cursor_position: app.input_cursor_visual_position(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LivePanelViewModel {
    pub phase: RunPhase,
    pub activity: Option<LiveActivityViewModel>,
    pub transcript_lines: Vec<Line<'static>>,
}

impl LivePanelViewModel {
    pub(crate) fn from_app(app: &AppState, transcript_rows: usize) -> Self {
        Self {
            phase: app.run_phase(),
            activity: app
                .live_activity_summary()
                .map(|summary| LiveActivityViewModel {
                    label: summary.label,
                    detail: summary.detail,
                }),
            transcript_lines: app.transcript_lines(transcript_rows),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LiveActivityViewModel {
    pub label: String,
    pub detail: String,
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
