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
mod tests {
    use std::{collections::BTreeMap, path::Path};

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use termquill_kernel::{
        AgentConfig, CompactionConfig, MemoryConfig, PermissionConfig, RootConfig, SessionConfig,
        WorkspaceConfig,
    };

    use super::*;

    fn test_config() -> RootConfig {
        RootConfig {
            workspace: WorkspaceConfig {
                root: ".".to_owned(),
            },
            session: SessionConfig {
                log_dir: ".termquill/sessions".to_owned(),
            },
            agent: AgentConfig {
                provider: "deepseek".to_owned(),
                model: "deepseek-v4-flash".to_owned(),
                max_turns: 8,
                tool_timeout_secs: 30,
            },
            permission: PermissionConfig::default(),
            memory: MemoryConfig { enabled: true },
            compaction: CompactionConfig::default(),
            providers: BTreeMap::new(),
            mcp_servers: Vec::new(),
        }
    }

    #[test]
    fn ui_view_model_projects_info_rail_and_composer_state() {
        let app = AppState::from_root_config(Path::new("/tmp/termquill.toml"), &test_config());
        let view_model = UiViewModel::from_app(&app);

        assert_eq!(view_model.composer.model_name, "deepseek-v4-flash");
        assert_eq!(view_model.composer.input_rows, 1);
        assert!(!view_model.info_rail.workspace_label.is_empty());
        assert!(
            view_model
                .info_rail
                .session_lines
                .iter()
                .any(|line| line == "provider: deepseek")
        );
        assert!(
            view_model
                .info_rail
                .controls
                .iter()
                .any(|line| line == "F1: keyboard help")
        );
        assert!(
            view_model
                .info_rail
                .controls
                .iter()
                .any(|line| line == "Ctrl-C: quit")
        );
    }

    #[test]
    fn composer_view_model_tracks_input_cursor() -> anyhow::Result<()> {
        let mut app = AppState::from_root_config(Path::new("/tmp/termquill.toml"), &test_config());
        app.handle_key_event(KeyEvent::new(KeyCode::Char('你'), KeyModifiers::NONE))?;
        app.handle_key_event(KeyEvent::new(KeyCode::Char('好'), KeyModifiers::NONE))?;

        let view_model = UiViewModel::from_app(&app);

        assert_eq!(view_model.composer.input, "你好");
        assert_eq!(view_model.composer.cursor_position, (4, 0));
        Ok(())
    }

    #[test]
    fn live_panel_view_model_projects_activity_and_transcript_rows() -> anyhow::Result<()> {
        let mut app = AppState::from_root_config(Path::new("/tmp/termquill.toml"), &test_config());
        app.set_terminal_size(120, 30);
        app.handle_key_event(KeyEvent::new(KeyCode::Char('你'), KeyModifiers::NONE))?;
        app.handle_key_event(KeyEvent::new(KeyCode::Char('好'), KeyModifiers::NONE))?;
        let _ = app.submit_input()?;

        let view_model = LivePanelViewModel::from_app(&app, 2);

        assert_eq!(
            view_model.activity,
            Some(LiveActivityViewModel {
                label: "thinking".to_owned(),
                detail: "reasoning with deepseek-v4-flash".to_owned(),
            })
        );
        assert!(view_model.transcript_lines.len() <= 2);
        Ok(())
    }
}
