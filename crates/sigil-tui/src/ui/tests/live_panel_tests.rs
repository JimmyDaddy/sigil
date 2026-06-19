use std::{collections::BTreeMap, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use sigil_kernel::{
    AgentConfig, CompactionConfig, MemoryConfig, PermissionConfig, RootConfig, SessionConfig,
    WorkspaceConfig,
};

use crate::{
    app::AppState,
    view_model::{
        LivePanelViewModel, LiveProgressViewModel, TaskStripRowViewModel, TaskStripViewModel,
    },
};

use super::*;
use crate::ui::theme::phase_accent;

fn test_config() -> RootConfig {
    RootConfig {
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        session: SessionConfig {
            log_dir: ".sigil/sessions".to_owned(),
        },
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        permission: PermissionConfig::default(),
        memory: MemoryConfig { enabled: true },
        skills: Default::default(),
        compaction: CompactionConfig::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        task: Default::default(),
        providers: BTreeMap::new(),
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
fn render_live_panel_keeps_wrapped_tail_visible() -> anyhow::Result<()> {
    let view_model = LivePanelViewModel {
        phase: crate::timeline::RunPhase::Idle,
        progress: None,
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
        progress: None,
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
        progress: Some(LiveProgressViewModel {
            title: "Thinking".to_owned(),
            detail: "reasoning with deepseek-v4-pro".to_owned(),
        }),
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
fn render_live_panel_keeps_long_task_label_expanded() -> anyhow::Result<()> {
    let view_model = LivePanelViewModel {
        phase: crate::timeline::RunPhase::Thinking,
        progress: None,
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
