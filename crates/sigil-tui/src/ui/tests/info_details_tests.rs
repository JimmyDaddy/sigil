use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use sigil_kernel::{EventHandler, RunEvent, UsageStats};

use super::*;
use crate::{
    app::{AppState, tests::common::test_config},
    surface::SurfaceState,
    ui::text::terminal_cell_width,
};

fn app_fixture() -> Result<(tempfile::TempDir, AppState)> {
    app_fixture_with_long_path(false)
}

fn app_fixture_with_long_path(long_path: bool) -> Result<(tempfile::TempDir, AppState)> {
    let root = tempfile::tempdir()?;
    let mut workspace = root
        .path()
        .join("项目  alpha")
        .join("complete-workspace-path");
    if long_path {
        for index in 0..24 {
            workspace.push(format!("workspace-long-segment-{index:02}"));
        }
    }
    std::fs::create_dir_all(&workspace)?;
    let mut config = test_config();
    config.workspace.root = workspace.display().to_string();
    let mut app = AppState::from_root_config(&root.path().join("sigil.toml"), &config);
    app.handle(RunEvent::Usage(UsageStats {
        prompt_tokens: 12345,
        completion_tokens: 678,
        ..UsageStats::default()
    }))?;
    Ok((root, app))
}

fn rendered_row(buffer: &Buffer, area: Rect, row: u16) -> String {
    let mut text = String::new();
    let mut column = area.x;
    while column < area.right() {
        let symbol = buffer[(column, row)].symbol();
        text.push_str(symbol);
        column = column.saturating_add(terminal_cell_width(symbol).max(1) as u16);
    }
    text
}

fn render(app: &AppState, width: u16, height: u16) -> Result<Buffer> {
    let surface = crate::surface_adapter::build_surface_model(
        Rect::new(0, 0, width, height),
        app,
        SurfaceState {
            frame_generation: 1,
            terminal_epoch: 1,
        },
    );
    assert!(surface.modal.info_details.is_some());
    let mut terminal = Terminal::new(TestBackend::new(width, height))?;
    terminal.draw(|frame| crate::ui::render_surface(frame, &surface))?;
    Ok(terminal.backend().buffer().clone())
}

fn press(app: &mut AppState, code: KeyCode) -> Result<()> {
    assert!(
        app.handle_key_event(KeyEvent::new(code, KeyModifiers::NONE))?
            .is_none()
    );
    Ok(())
}

fn assert_details_document_reachable(width: u16, height: u16) -> Result<()> {
    let (_root, mut app) = app_fixture_with_long_path(true)?;
    app.set_terminal_size(width, height);
    app.handle_key_event(KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE))?;
    let source = app.modal_lines();
    let full_path = app.workspace_root.display().to_string();
    assert!(source.contains(&full_path));
    let usage = "total spent: not estimated (pricing unavailable)";
    assert!(source.iter().any(|line| line == usage));
    let geometry = details_geometry(Rect::new(0, 0, width, height), &source);
    let rows = details_rows(&source, geometry.body.width as usize, None);
    let plain_rows = rows.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert!(plain_rows.concat().contains(&full_path));
    assert!(plain_rows.concat().contains(usage));
    let (max_scroll, page_rows) = info_details_scroll_bounds(width, height, &source);
    assert_eq!(max_scroll, rows.len().saturating_sub(page_rows));
    assert!(max_scroll > 0);
    let footer = FOOTER.to_owned()
        + &" ".repeat(geometry.footer.width as usize - terminal_cell_width(FOOTER));
    let mut covered = vec![false; rows.len()];
    loop {
        let scroll = app.info_details_view().expect("details").scroll_rows;
        let buffer = render(&app, width, height)?;
        assert_eq!(
            rendered_row(&buffer, geometry.footer, geometry.footer.y),
            footer
        );
        for row in 0..geometry.body.height {
            let index = scroll + row as usize;
            let expected = plain_rows.get(index).cloned().unwrap_or_default();
            let padded = expected.clone()
                + &" ".repeat(geometry.body.width as usize - terminal_cell_width(&expected));
            assert_eq!(
                rendered_row(&buffer, geometry.body, geometry.body.y + row),
                padded,
                "visual row {index} at {width}x{height}"
            );
            if let Some(seen) = covered.get_mut(index) {
                *seen = true;
            }
        }
        if scroll == max_scroll {
            break;
        }
        press(&mut app, KeyCode::PageDown)?;
    }
    assert!(
        covered.into_iter().all(|seen| seen),
        "all detail rows must be reachable"
    );
    press(&mut app, KeyCode::End)?;
    assert_eq!(
        app.info_details_view().expect("details").scroll_rows,
        max_scroll
    );
    press(&mut app, KeyCode::Home)?;
    assert_eq!(app.info_details_view().expect("details").scroll_rows, 0);
    Ok(())
}

#[test]
fn info_details_80x24_reaches_full_workspace_and_complete_costs() -> Result<()> {
    assert_details_document_reachable(80, 24)
}

#[test]
fn info_details_124x32_keeps_fixed_footer_and_reaches_the_complete_document() -> Result<()> {
    assert_details_document_reachable(124, 32)
}

#[test]
fn info_details_rows_preserve_wide_unicode_real_spaces_and_exact_tail_without_double_wrap()
-> Result<()> {
    let source = format!("workspace: /项目  alpha/{}TAIL", "目录  beta/".repeat(80));
    let lines = vec![source.clone()];
    let geometry = details_geometry(Rect::new(0, 0, 80, 12), &lines);
    let rows = details_rows(&lines, geometry.body.width as usize, None);
    let actual_source = rows.iter().map(ToString::to_string).collect::<String>();
    assert_eq!(actual_source, source);
    assert!(
        rows.iter()
            .all(|row| terminal_cell_width(&row.to_string()) <= geometry.body.width as usize)
    );
    let (max_scroll, page_rows) = info_details_scroll_bounds(80, 12, &lines);
    assert_eq!(max_scroll, rows.len() - page_rows);
    let (_root, app) = app_fixture()?;
    let visual = super::super::modal_visual(&app);
    let mut terminal = Terminal::new(TestBackend::new(80, 12))?;
    terminal.draw(|frame| {
        render_info_details(
            frame,
            &lines,
            &InfoDetailsSurface {
                scroll_rows: usize::MAX,
            },
            &visual,
        )
    })?;
    let buffer = terminal.backend().buffer();
    let body = (geometry.body.y..geometry.body.bottom())
        .map(|row| rendered_row(buffer, geometry.body, row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(body.contains("TAIL"));
    assert!(rendered_row(buffer, geometry.footer, geometry.footer.y).contains("Esc/Enter close"));
    Ok(())
}

#[test]
fn info_details_buffer_reader_skips_only_wide_occupancy_and_keeps_real_spaces() {
    let mut buffer = Buffer::empty(Rect::new(0, 0, 12, 1));
    buffer.set_string(0, 0, "中  A🙂 B", Style::default());
    assert_eq!(
        rendered_row(&buffer, Rect::new(0, 0, 12, 1), 0),
        "中  A🙂 B   "
    );
}

#[test]
fn info_details_f4_is_visible_and_opens_the_same_surface_without_a_rail_at_80x24() -> Result<()> {
    let (_root, mut app) = app_fixture()?;
    app.set_terminal_size(80, 24);
    let surface = crate::surface_adapter::build_surface_model(
        Rect::new(0, 0, 80, 24),
        &app,
        SurfaceState {
            frame_generation: 1,
            terminal_epoch: 1,
        },
    );
    assert_eq!(surface.layout.info_rail.width, 0);
    let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
    terminal.draw(|frame| crate::ui::render_surface(frame, &surface))?;
    let footer = rendered_row(
        terminal.backend().buffer(),
        surface.layout.footer,
        surface.layout.footer.y,
    );
    assert!(footer.contains("F4 details"), "visible footer: {footer}");
    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE))?
            .is_none()
    );
    assert_eq!(app.modal_title(), Some("Details"));
    assert!(
        app.modal_lines()
            .contains(&format!("session: {}", app.session_id))
    );
    press(&mut app, KeyCode::End)?;
    let source = app.modal_lines();
    let (max_scroll, _) = info_details_scroll_bounds(80, 24, &source);
    assert_eq!(
        app.info_details_view().expect("details").scroll_rows,
        max_scroll
    );
    let buffer = render(&app, 80, 24)?;
    let geometry = details_geometry(Rect::new(0, 0, 80, 24), &source);
    let last_page = (geometry.body.y..geometry.body.bottom())
        .map(|row| rendered_row(&buffer, geometry.body, row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(last_page.contains("total spent: not estimated (pricing unavailable)"));
    assert!(last_page.contains("balance:"));
    Ok(())
}
