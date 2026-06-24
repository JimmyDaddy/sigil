use std::{env, path::Path};

use ratatui::text::Line;

use crate::{
    app::{AppState, ComposerQueueAction, PaneFocus},
    commands::{global_control_hints, tool_card_control_hints},
    timeline::{ComposerQueueRow, RunPhase, SidebarAgentRow},
    ui::StatusKind,
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
    pub task_lines: Vec<String>,
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
                        "{} {}: {} {}",
                        row.focus_symbol(true),
                        row.label,
                        row.status_symbol(),
                        row.compact_detail()
                    )
                })
                .collect(),
            mcp_lines: app.mcp_sidebar_lines(),
            code_lines: app.code_intelligence_sidebar_lines(),
            task_lines: app.task_sidebar_lines(),
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
    pub agent_rows: Vec<SidebarAgentRow>,
    pub agent_panel_focused: bool,
    pub input: String,
    pub input_rows: u16,
    pub cursor_position: (u16, u16),
}

impl ComposerViewModel {
    fn from_app(app: &AppState) -> Self {
        Self {
            mode_label: format!(
                "{} · agent: {}",
                app.composer_mode_label(),
                app.active_agent_label()
            ),
            phase: app.run_phase(),
            provider_name: app.provider_name.clone(),
            model_name: app.model_name.clone(),
            reasoning_effort_label: app.reasoning_effort_label().to_owned(),
            agent_rows: app.composer_agent_rows(),
            agent_panel_focused: app.is_composer_agent_panel_focused(),
            input: app.composer_display_input(),
            input_rows: app.composer_input_rows(),
            cursor_position: app.input_cursor_visual_position(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TaskStripViewModel {
    pub title: String,
    pub detail: String,
    pub rows: Vec<TaskStripRowViewModel>,
}

impl TaskStripViewModel {
    pub(crate) fn from_task_strip_view(view: crate::app::task_sidebar::TaskStripView) -> Self {
        Self {
            title: view.title,
            detail: view.detail,
            rows: view
                .rows
                .into_iter()
                .map(|row| TaskStripRowViewModel {
                    kind: row.kind,
                    label: row.label,
                    active: row.active,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TaskStripRowViewModel {
    pub kind: StatusKind,
    pub label: String,
    pub active: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct FooterViewModel {
    pub phase: RunPhase,
    pub is_busy: bool,
    pub run_label: String,
    pub hints: String,
    pub context_label: String,
}

#[derive(Debug, Clone)]
pub(crate) struct QueueActionButtonViewModel {
    pub label: String,
    pub detail: String,
    pub selected: bool,
    pub destructive: bool,
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
    let agent = format!("agent: {}", app.active_agent_label());
    if app.pending_plan_approval().is_some() {
        return format!("{agent} · A ask · W workspace edits · C continue · Esc discard");
    }
    if app.pending_approval.is_some() {
        return format!("{agent} · Y allow · N deny · V diff");
    }
    if app.is_busy && matches!(app.run_phase(), RunPhase::Agent(_)) {
        return format!(
            "{agent} · Enter queue next turn · Ctrl-B background · Esc interrupt · Ctrl-T details"
        );
    }
    if app.is_busy {
        return format!("{agent} · Enter queue next turn · Esc interrupt · Ctrl-T details");
    }
    if app.active_pane == PaneFocus::Composer && app.has_slash_selector() {
        if app.has_agent_mention_selector() {
            return format!("{agent} · ↑↓ choose · Tab/Enter insert · Esc close");
        }
        return format!("{agent} · ↑↓ choose · Tab accept · Enter run · Esc close");
    }
    if app.is_composer_queue_panel_focused() {
        return format!("{agent} · Queue ↑↓ item · Tab action · Enter run · Esc input");
    }
    if app.is_composer_agent_panel_focused() {
        return format!("{agent} · ↑↓ agent · Enter switch · C close · M message · Esc input");
    }
    format!("{agent} · Enter send · Shift-Enter newline · Alt-A agent · / commands")
}

#[derive(Debug, Clone)]
pub(crate) struct LivePanelViewModel {
    pub phase: RunPhase,
    pub queue_rows: Vec<ComposerQueueRow>,
    pub queue_paused: bool,
    pub queue_panel_focused: bool,
    pub queue_action_buttons: Vec<QueueActionButtonViewModel>,
    pub progress: Option<LiveProgressViewModel>,
    pub plan_approval: Option<PlanApprovalViewModel>,
    pub task_strip: Option<TaskStripViewModel>,
    pub transcript_lines: Vec<Line<'static>>,
}

impl LivePanelViewModel {
    pub(crate) fn from_app(app: &AppState, transcript_rows: usize) -> Self {
        Self {
            phase: app.live_panel_phase(),
            queue_rows: app.composer_queue_rows(),
            queue_paused: app.composer_queue_paused(),
            queue_panel_focused: app.is_composer_queue_panel_focused(),
            queue_action_buttons: queue_action_buttons(app.selected_composer_queue_action()),
            progress: app
                .live_activity_summary()
                .map(|summary| LiveProgressViewModel::from_parts(&summary.label, &summary.detail)),
            plan_approval: app
                .pending_plan_approval()
                .map(PlanApprovalViewModel::from_pending),
            task_strip: app
                .task_strip_view()
                .map(TaskStripViewModel::from_task_strip_view),
            transcript_lines: app.transcript_lines(transcript_rows),
        }
    }
}

fn queue_action_buttons(selected: ComposerQueueAction) -> Vec<QueueActionButtonViewModel> {
    ComposerQueueAction::ORDER
        .iter()
        .map(|action| QueueActionButtonViewModel {
            label: action.label().to_owned(),
            detail: action.detail().to_owned(),
            selected: *action == selected,
            destructive: action.is_destructive(),
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanApprovalViewModel {
    pub hash: String,
    pub scope_summary: String,
}

impl PlanApprovalViewModel {
    fn from_pending(pending: &crate::app::PendingPlanApproval) -> Self {
        Self {
            hash: short_plan_hash(&pending.plan_hash),
            scope_summary: pending.scope_summary.clone(),
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

fn short_plan_hash(plan_hash: &str) -> String {
    plan_hash.chars().take(19).collect()
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

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/view_model_tests.rs"]
mod tests;
