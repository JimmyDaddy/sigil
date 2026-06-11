use std::{collections::BTreeMap, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use sigil_kernel::{
    AgentConfig, CompactionConfig, MemoryConfig, PermissionConfig, RootConfig, SessionConfig,
    WorkspaceConfig,
};

use crate::{app::AppState, view_model::LivePanelViewModel};

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
        compaction: CompactionConfig::default(),
        code_intelligence: Default::default(),
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
fn live_spinner_frame_uses_visible_block_pulse() {
    let frame = live_spinner_frame();

    assert!(frame.chars().count() >= 4);
    assert!(frame.contains('▰'));
    assert!(frame.contains('▱'));
}
