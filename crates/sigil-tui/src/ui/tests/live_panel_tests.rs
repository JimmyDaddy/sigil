use std::{collections::BTreeMap, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use sigil_kernel::{
    AgentConfig, CompactionConfig, MemoryConfig, PermissionConfig, RootConfig, SessionConfig,
    WorkspaceConfig,
};

use crate::{
    app::AppState,
    timeline::ComposerQueueRow,
    ui::StatusKind,
    view_model::{
        LivePanelViewModel, LiveProgressViewModel, PlanApprovalViewModel,
        QueueActionButtonViewModel, TaskStripRowViewModel, TaskStripViewModel,
    },
};

use super::*;
use crate::ui::theme::{accent_blue, phase_accent};

fn test_config() -> RootConfig {
    RootConfig {
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        storage: Default::default(),
        session: SessionConfig {
            log_dir: Some(".sigil/sessions".to_owned()),
        },
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        model_request: Default::default(),
        permission: PermissionConfig::default(),
        memory: MemoryConfig { enabled: true },
        skills: Default::default(),
        compaction: CompactionConfig::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: Default::default(),
        providers: BTreeMap::new(),
        web: Default::default(),
        mcp_servers: Vec::new(),
    }
}

#[test]
fn render_live_progress_lines_shows_current_phase() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("/tmp/sigil.toml"), &test_config());
    app.set_terminal_size(120, 30);
    app.handle_key_event(KeyEvent::new(KeyCode::Char('你'), KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('好'), KeyModifiers::NONE))?;
    let _ = app.submit_input()?;

    let view_model = LivePanelViewModel::from_app(&app, 4);
    let lines = render_live_progress_lines(
        view_model
            .progress
            .as_ref()
            .expect("busy run should expose live progress"),
        phase_accent(&view_model.phase),
    );
    let plain = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();

    assert!(plain.contains("Thinking..."));
    assert!(!plain.contains("(Thinking)"));
    assert!(plain.contains("reasoning with"));
    Ok(())
}

#[test]
fn status_band_line_pads_tail_with_band_background() {
    let theme = Theme::default();
    let bg = theme.palette.surface_panel_alt;
    let line = status_band_line(Line::from(vec![Span::raw("Queue")]), 12, bg);

    assert_eq!(line_display_width(&line.spans), 12);
    assert_eq!(line.style.bg, Some(bg));
    assert_eq!(line.spans[0].style.bg, Some(bg));
    let tail = line.spans.last().expect("expected padded tail span");
    assert_eq!(tail.content.as_ref(), "       ");
    assert_eq!(tail.style.bg, Some(bg));
}

#[test]
fn phase_accent_uses_blue_for_agent_phase() {
    assert_eq!(
        phase_accent(&crate::timeline::RunPhase::Agent("explore".to_owned())),
        accent_blue()
    );
}

#[test]
fn render_live_panel_keeps_wrapped_tail_visible() -> anyhow::Result<()> {
    let view_model = LivePanelViewModel {
        phase: crate::timeline::RunPhase::Idle,
        queue_rows: Vec::new(),
        queue_paused: false,
        queue_panel_focused: false,
        queue_action_buttons: Vec::new(),
        progress: None,
        plan_approval: None,
        task_strip: None,
        transcript_lines: vec![Line::from(
            "prefix words that wrap across rows before visible TAIL",
        )],
    };
    let backend = TestBackend::new(16, 4);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_live_panel(frame, frame.area(), &view_model))?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("TAIL"));
    Ok(())
}

#[test]
fn render_live_panel_keeps_bottom_padding_clear() -> anyhow::Result<()> {
    let view_model = LivePanelViewModel {
        phase: crate::timeline::RunPhase::Idle,
        queue_rows: Vec::new(),
        queue_paused: false,
        queue_panel_focused: false,
        queue_action_buttons: Vec::new(),
        progress: None,
        plan_approval: None,
        task_strip: None,
        transcript_lines: vec![Line::from("visible tail")],
    };
    let backend = TestBackend::new(16, 4);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_live_panel(frame, frame.area(), &view_model))?;

    let buffer = terminal.backend().buffer();
    let bottom_row = buffer.content()[48..64]
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(bottom_row.trim().is_empty());
    Ok(())
}

#[test]
fn render_live_panel_merges_task_strip_into_status_band() -> anyhow::Result<()> {
    let view_model = LivePanelViewModel {
        phase: crate::timeline::RunPhase::Thinking,
        queue_rows: Vec::new(),
        queue_paused: false,
        queue_panel_focused: false,
        queue_action_buttons: Vec::new(),
        progress: Some(LiveProgressViewModel {
            title: "Thinking".to_owned(),
            detail: "reasoning with deepseek-v4-pro".to_owned(),
        }),
        plan_approval: None,
        task_strip: Some(TaskStripViewModel {
            title: "Task task_1".to_owned(),
            detail: "running · v1 · 1/2 done".to_owned(),
            rows: vec![
                TaskStripRowViewModel {
                    kind: crate::ui::StatusKind::Success,
                    label: "1. inspect layout".to_owned(),
                    active: false,
                },
                TaskStripRowViewModel {
                    kind: crate::ui::StatusKind::Pending,
                    label: "2. update status band".to_owned(),
                    active: true,
                },
            ],
        }),
        transcript_lines: vec![Line::from("visible tail")],
    };
    let backend = TestBackend::new(104, 8);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_live_panel(frame, frame.area(), &view_model))?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("visible tail"));
    assert!(rendered.contains("Thinking..."));
    assert!(rendered.contains("Task task_1"));
    assert!(rendered.contains("running · v1 · 1/2 done"));
    assert!(rendered.contains("✓ 1. inspect layout"));
    assert!(rendered.contains("◇ 2. update status band"));
    assert!(rendered.contains("▌"));
    assert!(!rendered.contains("status:"));
    Ok(())
}

#[test]
fn render_live_panel_shows_queue_strip_actions_above_status() -> anyhow::Result<()> {
    let view_model = LivePanelViewModel {
        phase: crate::timeline::RunPhase::Idle,
        queue_rows: vec![ComposerQueueRow {
            label: "queued prompt".to_owned(),
            detail: "queued · chat".to_owned(),
            status: StatusKind::Pending,
            selected: true,
        }],
        queue_paused: false,
        queue_panel_focused: true,
        queue_action_buttons: vec![
            QueueActionButtonViewModel {
                label: "Run next".to_owned(),
                detail: "run after the current turn".to_owned(),
                selected: true,
                destructive: false,
            },
            QueueActionButtonViewModel {
                label: "Interrupt".to_owned(),
                detail: "stop current turn and run this follow-up".to_owned(),
                selected: false,
                destructive: false,
            },
            QueueActionButtonViewModel {
                label: "Edit".to_owned(),
                detail: "edit follow-up".to_owned(),
                selected: false,
                destructive: false,
            },
            QueueActionButtonViewModel {
                label: "Delete".to_owned(),
                detail: "remove follow-up".to_owned(),
                selected: false,
                destructive: true,
            },
        ],
        progress: Some(LiveProgressViewModel {
            title: "Thinking".to_owned(),
            detail: "reasoning with deepseek-v4-pro".to_owned(),
        }),
        plan_approval: None,
        task_strip: None,
        transcript_lines: vec![Line::from("visible tail")],
    };
    let backend = TestBackend::new(104, 8);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_live_panel(frame, frame.area(), &view_model))?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Follow-ups"));
    assert!(rendered.contains("queued prompt"));
    assert!(rendered.contains("Run next"));
    assert!(rendered.contains("Interrupt"));
    assert!(rendered.contains("Edit"));
    assert!(rendered.contains("Delete"));
    assert!(rendered.contains("Thinking..."));
    assert!(!rendered.contains("S now"));
    assert!(!rendered.contains("D delete"));
    assert!(!rendered.contains("E edit"));
    Ok(())
}

#[test]
fn render_live_panel_queue_strip_covers_paused_and_unfocused_rows() -> anyhow::Result<()> {
    let view_model = LivePanelViewModel {
        phase: crate::timeline::RunPhase::Idle,
        queue_rows: vec![
            ComposerQueueRow {
                label: "first queued prompt".to_owned(),
                detail: "paused · chat".to_owned(),
                status: StatusKind::Pending,
                selected: false,
            },
            ComposerQueueRow {
                label: "second queued prompt".to_owned(),
                detail: "queued · chat".to_owned(),
                status: StatusKind::Running,
                selected: true,
            },
        ],
        queue_paused: true,
        queue_panel_focused: false,
        queue_action_buttons: vec![QueueActionButtonViewModel {
            label: "Interrupt".to_owned(),
            detail: "stop current turn and run this follow-up".to_owned(),
            selected: false,
            destructive: false,
        }],
        progress: None,
        plan_approval: None,
        task_strip: None,
        transcript_lines: vec![Line::from("visible tail")],
    };
    let backend = TestBackend::new(96, 8);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_live_panel(frame, frame.area(), &view_model))?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Follow-ups paused"));
    assert!(rendered.contains("/queue advanced"));
    assert!(rendered.contains("first queued prompt"));
    assert!(rendered.contains("second queued prompt"));
    assert!(rendered.contains("Interrupt"));
    Ok(())
}

#[test]
fn render_live_panel_shows_plan_approval_surface() -> anyhow::Result<()> {
    let view_model = LivePanelViewModel {
        phase: crate::timeline::RunPhase::Idle,
        queue_rows: Vec::new(),
        queue_paused: false,
        queue_panel_focused: false,
        queue_action_buttons: Vec::new(),
        progress: None,
        plan_approval: Some(PlanApprovalViewModel {
            summary: "inspect and edit with preview".to_owned(),
            steps: vec!["inspect and edit with preview".to_owned()],
            target_paths: vec!["src/lib.rs".to_owned(), "README.md".to_owned()],
            suggested_checks: vec!["cargo test".to_owned()],
            target_path_count: 2,
            suggested_check_count: 1,
        }),
        task_strip: None,
        transcript_lines: vec![Line::from("plan body")],
    };
    let backend = TestBackend::new(96, 9);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_live_panel(frame, frame.area(), &view_model))?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Plan"));
    assert!(rendered.contains("ready"));
    assert!(rendered.contains("structured plan"));
    assert!(rendered.contains("2 paths"));
    assert!(rendered.contains("1 check"));
    assert!(rendered.contains("inspect and edit"));
    assert!(rendered.contains("Enter"));
    assert!(rendered.contains("create and run task"));
    assert!(!rendered.contains("scoped edits"));
    assert!(!rendered.contains("Shift-Enter"));
    assert!(!rendered.contains("revise"));
    assert!(rendered.contains("Esc discard"));
    Ok(())
}

#[test]
fn render_live_panel_keeps_long_task_label_expanded() -> anyhow::Result<()> {
    let view_model = LivePanelViewModel {
        phase: crate::timeline::RunPhase::Thinking,
        queue_rows: Vec::new(),
        queue_paused: false,
        queue_panel_focused: false,
        queue_action_buttons: Vec::new(),
        progress: None,
        plan_approval: None,
        task_strip: Some(TaskStripViewModel {
            title: "Task task_3".to_owned(),
            detail: "started".to_owned(),
            rows: vec![TaskStripRowViewModel {
                kind: crate::ui::StatusKind::Running,
                label: "1. 输出一个冷笑话2、解释一下这个冷笑话为什么好笑".to_owned(),
                active: true,
            }],
        }),
        transcript_lines: vec![Line::from("visible tail")],
    };
    let backend = TestBackend::new(96, 6);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_live_panel(frame, frame.area(), &view_model))?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    let compact = rendered.replace(' ', "");
    assert!(compact.contains("1.输出一个冷笑话2、解释一下这个冷笑话为什么好笑"));
    Ok(())
}
