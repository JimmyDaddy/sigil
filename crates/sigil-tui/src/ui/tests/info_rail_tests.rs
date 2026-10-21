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
        workspace_label: "project".to_owned(),
        workspace_git_status: crate::workspace_git::parse_workspace_git_status(concat!(
            "## feature/tui-status...origin/feature/tui-status [ahead 2]\n",
            "M  staged.rs\n",
            " M modified.rs\n",
            "?? new.rs\n",
        )),
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
fn render_info_rail_skips_empty_optional_sections() -> anyhow::Result<()> {
    let mut model = sample_view_model();
    model.agent_lines.clear();
    model.task_lines.clear();
    model.mcp_lines.clear();
    model.code_lines.clear();
    model.usage_lines.clear();
    let mut terminal = Terminal::new(TestBackend::new(54, 32))?;
    terminal.draw(|frame| render_info_rail(frame, frame.area(), &model))?;
    let text = rendered_text(&terminal);
    assert!(text.contains("project"));
    assert!(text.contains("mode: ready"));
    assert!(text.contains("attention"));
    assert!(text.contains("Alt-I details"));
    assert!(!text.contains("permissions"));
    assert!(!text.contains("controls"));
    for hidden in ["agents", "task", "MCP", "LSP", "usage"] {
        assert!(
            !text.contains(hidden),
            "empty section {hidden} must not occupy the rail"
        );
    }
    Ok(())
}

#[test]
fn render_info_rail_renders_sections_and_content() -> anyhow::Result<()> {
    let backend = TestBackend::new(54, 32);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| {
        render_info_rail(frame, Rect::new(0, 0, 54, 32), &sample_view_model());
    })?;

    let rendered = rendered_text(&terminal);
    assert!(rendered.contains("project"));
    assert!(rendered.contains("git:"));
    assert!(rendered.contains("feature/tui-status"));
    assert!(rendered.contains("3 changes"));
    assert!(!rendered.contains("stg 1"));
    assert!(!rendered.contains("mod 1"));
    assert!(!rendered.contains("new 1"));
    assert!(rendered.contains("↑2"));
    assert!(rendered.contains("session"));
    assert!(rendered.contains("attention"));
    assert!(rendered.contains("agents"));
    assert!(rendered.contains("task"));
    assert!(rendered.contains("MCP"));
    assert!(rendered.contains("filesystem"));
    assert!(rendered.contains("LSP"));
    assert!(!rendered.contains("tokens: 10/100"));
    assert!(!rendered.contains("controls"));
    assert!(rendered.contains("Alt-I details"));
    assert!(rendered.contains("mode"));
    assert!(rendered.contains("approval"));
    Ok(())
}

#[test]
fn render_info_rail_keeps_git_total_without_normal_change_breakdown() -> anyhow::Result<()> {
    let mut view_model = sample_view_model();
    view_model.workspace_git_status = crate::workspace_git::parse_workspace_git_status(&format!(
        "## main\n{}{}{}",
        "M  staged.rs\n".repeat(12),
        " M modified.rs\n".repeat(73),
        "?? new.rs\n".repeat(16),
    ));
    let backend = TestBackend::new(38, 18);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| {
        render_info_rail(frame, Rect::new(0, 0, 38, 18), &view_model);
    })?;

    let row = |y: u16| {
        (0..38)
            .map(|x| terminal.backend().buffer()[(x, y)].symbol())
            .collect::<String>()
    };
    assert!(
        row(2).contains("git: main · 101 changes"),
        "summary row: {:?}",
        row(2)
    );
    assert!(row(3).contains("mode: ready"));
    let text = rendered_text(&terminal);
    assert!(!text.contains("stg 12"));
    assert!(!text.contains("mod 73"));
    assert!(!text.contains("new 16"));
    assert!(!row(2).contains("..."));
    Ok(())
}

#[test]
fn render_info_rail_preserves_branch_and_total_at_minimum_rail_width() -> anyhow::Result<()> {
    let mut view_model = sample_view_model();
    view_model.session_title = "short".to_owned();
    view_model.workspace_label = "/repo".to_owned();
    view_model.workspace_git_status = crate::workspace_git::parse_workspace_git_status(&format!(
        "## main\n{}{}{}",
        "M  staged.rs\n".repeat(12),
        " M modified.rs\n".repeat(73),
        "?? new.rs\n".repeat(16),
    ));
    let backend = TestBackend::new(24, 18);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| {
        render_info_rail(frame, Rect::new(0, 0, 24, 18), &view_model);
    })?;

    let row = |y: u16| {
        (0..24)
            .map(|x| terminal.backend().buffer()[(x, y)].symbol())
            .collect::<String>()
    };
    assert!(row(2).contains("git: main"));
    assert!(row(3).contains("101 changes"));
    assert!(row(4).contains("mode: ready"));
    assert!(!rendered_text(&terminal).contains("S12 M73 ?16"));
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
fn info_rail_detail_entry_stays_visible_when_attention_fills_the_body() -> anyhow::Result<()> {
    let mut model = sample_view_model();
    model.permission_lines = vec!["save failed: configuration changed on disk".to_owned()];
    model.code_lines = (0..40)
        .map(|index| format!("src/file_{index}.rs: 1 error"))
        .collect();
    for height in [12, 24] {
        let area = Rect::new(0, 0, 32, height);
        let mut terminal = Terminal::new(TestBackend::new(32, height))?;
        terminal.draw(|frame| render_info_rail(frame, area, &model))?;
        let details = info_rail_details_area(area).expect("details entry");
        let row = (details.x..details.right())
            .map(|x| terminal.backend().buffer()[(x, details.y)].symbol())
            .collect::<String>();
        assert!(row.contains("Alt-I details"), "{row:?}");
        assert!(details.y >= info_rail_body_area(area).bottom());
        assert!(rendered_text(&terminal).contains("save failed:"));
    }
    Ok(())
}

#[test]
fn info_rail_agent_offsets_match_rendered_rows_after_compact_header() -> anyhow::Result<()> {
    for width in [32, 54] {
        let model = sample_view_model();
        let area = Rect::new(0, 0, width, 24);
        let body = info_rail_body_area(area);
        let layout = info_rail_body_layout(&model, body.width as usize, &Theme::default());
        let mut terminal = Terminal::new(TestBackend::new(width, 24))?;
        terminal.draw(|frame| render_info_rail(frame, area, &model))?;
        for (offset, label) in layout.agent_row_offsets.iter().zip(["main", "helper"]) {
            assert!(*offset < body.height as usize);
            let y = body.y + *offset as u16;
            let row = (body.x..body.right())
                .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                .collect::<String>();
            assert!(row.contains(label), "expected {label}: {row:?}");
            assert_ne!(y, info_rail_details_area(area).expect("details entry").y);
        }
    }
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
    assert_eq!(
        muted.spans[1].style,
        Style::default().fg(crate::ui::theme::default_palette().status_pending)
    );
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
            .fg(crate::ui::theme::default_palette().status_thinking)
            .add_modifier(Modifier::BOLD)
    );
    assert_eq!(
        running.spans[2].style,
        Style::default()
            .fg(crate::ui::theme::default_palette().status_thinking)
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
    assert_eq!(
        pending.spans[1].style,
        Style::default().fg(crate::ui::theme::default_palette().status_pending)
    );
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
