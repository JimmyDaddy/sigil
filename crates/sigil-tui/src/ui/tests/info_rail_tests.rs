use ratatui::{
    Terminal,
    backend::TestBackend,
    layout::Rect,
    style::{Modifier, Style},
};

use crate::view_model::InfoRailViewModel;

use super::*;

fn sample_view_model() -> InfoRailViewModel {
    InfoRailViewModel {
        session_title: "Session title that is deliberately longer than the rail".to_owned(),
        workspace_label: "/tmp/project/with/a/very/long/path".to_owned(),
        session_lines: vec!["mode: ready".to_owned()],
        permission_lines: vec!["approval: ask".to_owned()],
        agent_lines: vec![
            "◉ main: ○ current session".to_owned(),
            "  helper: ◇ idle".to_owned(),
        ],
        task_lines: vec!["task: task_1".to_owned(), "status: running".to_owned()],
        mcp_lines: vec!["filesystem: deferred".to_owned()],
        code_lines: vec!["server: rust-analyzer".to_owned()],
        usage_lines: vec!["tokens: 10/100".to_owned()],
        controls: vec!["Enter send".to_owned()],
    }
}

fn rendered_text(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn render_info_rail_renders_sections_and_content() -> anyhow::Result<()> {
    let backend = TestBackend::new(54, 32);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| {
        render_info_rail(frame, Rect::new(0, 0, 54, 32), &sample_view_model());
    })?;

    let rendered = rendered_text(&terminal);
    assert!(rendered.contains("info"));
    assert!(rendered.contains("session"));
    assert!(rendered.contains("permissions"));
    assert!(rendered.contains("agents"));
    assert!(rendered.contains("task"));
    assert!(rendered.contains("MCP"));
    assert!(rendered.contains("filesystem"));
    assert!(rendered.contains("LSP"));
    assert!(rendered.contains("usage"));
    assert!(rendered.contains("controls"));
    assert!(rendered.contains("mode"));
    assert!(rendered.contains("approval"));
    Ok(())
}

#[test]
fn render_info_rail_returns_early_when_inner_rect_disappears() -> anyhow::Result<()> {
    let backend = TestBackend::new(6, 2);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| {
        render_info_rail(frame, Rect::new(0, 0, 6, 2), &sample_view_model());
    })?;

    assert!(!rendered_text(&terminal).contains("info"));
    Ok(())
}

#[test]
fn render_info_line_formats_labels_markers_and_plain_values() {
    let label = render_info_line("mode: ready", 32);
    assert_eq!(label.spans[1].content.as_ref(), "mode:");
    assert_eq!(
        label.spans[1].style,
        Style::default()
            .fg(super::dim())
            .add_modifier(Modifier::BOLD)
    );

    let selected = render_info_line("◉ primary agent", 32);
    assert_eq!(selected.spans[1].content.as_ref(), "◉ ");
    assert_eq!(
        selected.spans[1].style,
        Style::default()
            .fg(super::accent_blue())
            .add_modifier(Modifier::BOLD)
    );

    let muted = render_info_line("◇ helper idle", 32);
    assert_eq!(muted.spans[1].content.as_ref(), "◇ ");
    assert_eq!(muted.spans[1].style, Style::default().fg(super::dim()));
    assert_eq!(
        muted.spans[2].style,
        Style::default().fg(crate::ui::theme::muted())
    );

    let running = render_info_line("◐ 2. running overview", 32);
    assert!(matches!(
        running.spans[1].content.as_ref(),
        "◐ " | "◓ " | "◑ " | "◒ "
    ));
    assert_eq!(
        running.spans[1].style,
        Style::default()
            .fg(super::accent_gold())
            .add_modifier(Modifier::BOLD)
    );
    assert_eq!(
        running.spans[2].style,
        Style::default()
            .fg(super::accent_gold())
            .add_modifier(Modifier::BOLD)
    );

    let failed = render_info_line("✕ 1. failed gate_check", 32);
    assert_eq!(failed.spans[1].content.as_ref(), "✕ ");
    assert_eq!(
        failed.spans[1].style,
        Style::default()
            .fg(super::accent_rose())
            .add_modifier(Modifier::BOLD)
    );
    assert_eq!(
        failed.spans[2].style,
        Style::default()
            .fg(super::accent_rose())
            .add_modifier(Modifier::BOLD)
    );

    let completed = render_info_line("✓ 1. completed gate_check", 32);
    assert_eq!(completed.spans[1].content.as_ref(), "✓ ");
    assert_eq!(
        completed.spans[1].style,
        Style::default().fg(super::accent_lime())
    );
    assert_eq!(
        completed.spans[2].style,
        Style::default().fg(super::accent_lime())
    );

    let interrupted = render_info_line("✕ 1. interrupted gate_check", 32);
    assert_eq!(interrupted.spans[1].content.as_ref(), "✕ ");
    assert_eq!(
        interrupted.spans[1].style,
        Style::default()
            .fg(super::accent_rose())
            .add_modifier(Modifier::BOLD)
    );
    assert_eq!(
        interrupted.spans[2].style,
        Style::default()
            .fg(super::accent_rose())
            .add_modifier(Modifier::BOLD)
    );

    let pending = render_info_line("◇ 2. pending overview", 32);
    assert_eq!(pending.spans[1].content.as_ref(), "◇ ");
    assert_eq!(pending.spans[1].style, Style::default().fg(super::dim()));
    assert_eq!(
        pending.spans[2].style,
        Style::default().fg(crate::ui::theme::muted())
    );

    let agent = render_info_line("◉ main: ○ current session", 32);
    assert_eq!(agent.spans[1].content.as_ref(), "◉ ");
    assert_eq!(agent.spans[2].content.as_ref(), "main:");
    assert_eq!(agent.spans[4].content.as_ref(), "○");

    let plain = render_info_line("plain value", 32);
    assert_eq!(plain.spans[1].content.as_ref(), "plain value");
    assert_eq!(plain.spans[1].style, Style::default().fg(super::ink()));
}

#[test]
fn render_info_line_with_theme_uses_configured_palette_for_markers() {
    let theme = crate::ui::theme::Theme::builtin(sigil_kernel::ThemeId::SolarizedLight);
    let palette = theme.palette.clone();

    let completed = render_info_line_with_theme("✓ ready", 32, &theme);
    assert_eq!(completed.spans[1].content.as_ref(), "✓ ");
    assert_eq!(
        completed.spans[1].style,
        Style::default().fg(palette.status_success)
    );
    assert_eq!(
        completed.spans[2].style,
        Style::default().fg(palette.status_success)
    );

    let labeled = render_info_line_with_theme("mode: ✓ ready", 32, &theme);
    assert_eq!(labeled.spans[3].content.as_ref(), "✓");
    assert_eq!(
        labeled.spans[3].style,
        Style::default().fg(palette.status_success)
    );
}
