use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

use crate::{
    app::{AppState, tests::common::test_config},
    surface::SurfaceState,
};

use super::*;

fn app_fixture() -> Result<(tempfile::TempDir, AppState)> {
    let root = tempfile::tempdir()?;
    let mut config = test_config();
    config.workspace.root = root.path().display().to_string();
    let app = AppState::from_root_config(&root.path().join("sigil.toml"), &config);
    Ok((root, app))
}

fn press(app: &mut AppState, code: KeyCode) -> Result<()> {
    assert!(
        app.handle_key_event(KeyEvent::new(code, KeyModifiers::NONE))?
            .is_none()
    );
    Ok(())
}

fn rendered_rows(app: &AppState, width: u16, height: u16) -> Result<Vec<String>> {
    let surface = crate::surface_adapter::build_surface_model(
        Rect::new(0, 0, width, height),
        app,
        SurfaceState {
            frame_generation: 1,
            terminal_epoch: 1,
        },
    );
    let mut terminal = Terminal::new(TestBackend::new(width, height))?;
    terminal.draw(|frame| crate::ui::render_surface(frame, &surface))?;
    let buffer = terminal.backend().buffer();
    Ok((0..height)
        .map(|y| rendered_row(buffer, y, width))
        .collect())
}

fn rendered_row(buffer: &Buffer, y: u16, width: u16) -> String {
    let mut text = String::new();
    let mut column = 0;
    while column < width {
        let symbol = buffer[(column, y)].symbol();
        text.push_str(symbol);
        column = column.saturating_add(terminal_cell_width(symbol).max(1) as u16);
    }
    text
}

#[test]
fn keyboard_help_buffer_reader_preserves_real_spaces_and_skips_wide_occupancy() {
    let mut buffer = Buffer::empty(Rect::new(0, 0, 10, 1));
    buffer.set_string(0, 0, "A、  B", Style::default());
    assert_eq!(rendered_row(&buffer, 0, 10), "A、  B    ");
}

fn assert_help_viewport(width: u16, height: u16) -> Result<()> {
    let (_root, mut app) = app_fixture()?;
    app.set_terminal_size(width, height);
    press(&mut app, KeyCode::F(1))?;
    let common = rendered_rows(&app, width, height)?.join("\n");
    assert!(common.contains("[Common]"));
    assert!(common.contains("All shortcuts"));
    assert!(common.contains("Ctrl-J"));
    assert!(common.contains("Ctrl-C"));
    assert!(common.contains("Esc/Enter close"));
    assert!(!common.contains("durable intent ownership"));
    press(&mut app, KeyCode::Tab)?;
    let top = rendered_rows(&app, width, height)?;
    let tab_row = top
        .iter()
        .position(|line| line.contains("[All shortcuts]"))
        .expect("fixed tabs");
    let footer_row = top
        .iter()
        .position(|line| line.contains("Esc/Enter close"))
        .expect("fixed footer");
    assert!(top.join("\n").contains("Session"));
    press(&mut app, KeyCode::PageDown)?;
    let page = rendered_rows(&app, width, height)?;
    assert_ne!(page, top);
    assert_eq!(page[tab_row], top[tab_row]);
    assert_eq!(page[footer_row], top[footer_row]);
    press(&mut app, KeyCode::End)?;
    let bottom = rendered_rows(&app, width, height)?;
    assert!(
        bottom
            .join("\n")
            .contains("Use / or 、 to open the command palette."),
        "actual bottom rows: {bottom:#?}",
    );
    let last_command = app
        .modal_lines()
        .into_iter()
        .rev()
        .find_map(|line| {
            line.starts_with('/')
                .then(|| line.split(':').next().expect("trigger").to_owned())
        })
        .expect("last slash command");
    assert!(bottom.join("\n").contains(&last_command));
    assert_eq!(bottom[tab_row], top[tab_row]);
    assert_eq!(bottom[footer_row], top[footer_row]);
    press(&mut app, KeyCode::Home)?;
    assert_eq!(rendered_rows(&app, width, height)?, top);
    Ok(())
}

#[test]
fn keyboard_help_80x24_keeps_common_priority_and_reaches_all_content() -> Result<()> {
    assert_help_viewport(80, 24)
}

#[test]
fn keyboard_help_144x44_keeps_fixed_tabs_and_footer_while_scrolling() -> Result<()> {
    assert_help_viewport(144, 44)
}

#[test]
fn keyboard_help_wraps_wide_characters_once_and_reaches_the_exact_tail() -> Result<()> {
    let source = format!("Command: {}TAIL", "中文 detail ".repeat(70));
    let lines = vec![source];
    let geometry = help_geometry(Rect::new(0, 0, 80, 12), &lines);
    let rows = help_rows(&lines, geometry.body.width as usize, None);
    assert!(rows.len() > geometry.body.height as usize);
    assert!(
        rows.iter()
            .all(|line| terminal_cell_width(&line.to_string()) <= geometry.body.width as usize)
    );
    let (max_scroll, page_rows) = keyboard_help_scroll_bounds(80, 12, &lines);
    assert_eq!(page_rows, geometry.body.height as usize);
    assert_eq!(max_scroll, rows.len() - page_rows);
    let (_root, app) = app_fixture()?;
    let visual = super::super::modal_visual(&app);
    let mut terminal = Terminal::new(TestBackend::new(80, 12))?;
    terminal.draw(|frame| {
        render_keyboard_help(
            frame,
            &lines,
            &KeyboardHelpSurface {
                page: KeyboardHelpPage::All,
                scroll_rows: usize::MAX,
            },
            &visual,
        )
    })?;
    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("TAIL"));
    assert!(rendered.contains("Esc/Enter close"));
    Ok(())
}

#[test]
fn keyboard_help_words_preserve_real_spacing_and_do_not_split_short_words() {
    let source = "Key:  Review the selected file before continuing.";
    let rows = help_rows(&[source.to_owned()], 18, None);
    assert_eq!(
        rows.iter().map(ToString::to_string).collect::<String>(),
        source
    );
    assert!(rows.iter().any(|row| row.to_string().contains("selected")));
    assert!(
        rows.iter()
            .all(|row| crate::ui::text::terminal_cell_width(&row.to_string()) <= 18)
    );
}

#[test]
fn keyboard_help_unlabelled_multiline_rows_preserve_blank_lines_and_real_spaces() {
    let lines = vec!["Read this first\n\nKeep  both spaces e\u{301}🙂\n".to_owned()];
    let rows = help_rows(&lines, 80, None);
    assert_eq!(
        rows.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["Read this first", "", "Keep  both spaces e\u{301}🙂", ""]
    );
}
