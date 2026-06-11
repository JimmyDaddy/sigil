use std::{collections::BTreeMap, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, style::Color};
use sigil_kernel::{
    AgentConfig, CompactionConfig, MemoryConfig, PermissionConfig, RootConfig, SessionConfig,
    WorkspaceConfig,
};

use super::super::theme::{
    config_border, config_primary, config_section_bg, config_selected_bg, config_tab_bg,
};
use super::*;

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
fn render_main_screen_shows_keyboard_help_modal() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE))?;
    let backend = TestBackend::new(96, 24);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Core shortcuts"));
    Ok(())
}

#[test]
fn render_main_screen_collapses_info_rail_on_narrow_terminals() -> anyhow::Result<()> {
    let app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(!rendered.contains("info"));
    assert!(rendered.contains("Build"));
    assert!(rendered.contains("ctx"));
    Ok(())
}

#[test]
fn render_main_screen_keeps_info_rail_on_wide_terminals() -> anyhow::Result<()> {
    let app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let backend = TestBackend::new(140, 24);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("info"));
    assert!(rendered.contains("session"));
    assert!(rendered.contains("LSP"));
    Ok(())
}

#[test]
fn render_main_screen_places_cursor_on_new_composer_line() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(80, 12);
    app.handle_key_event(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT))?;
    let backend = TestBackend::new(80, 12);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    terminal.backend_mut().assert_cursor_position((3, 9));
    Ok(())
}

#[test]
fn render_main_screen_keeps_composer_text_visible() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(80, 12);
    for character in "visible text".chars() {
        app.handle_key_event(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))?;
    }
    let backend = TestBackend::new(80, 12);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("visible text"));
    Ok(())
}

#[test]
fn footer_context_width_uses_half_width_cap_and_hides_small_areas() {
    let footer = FooterViewModel {
        phase: crate::timeline::RunPhase::Idle,
        is_busy: false,
        run_label: "ready".to_owned(),
        hints: String::new(),
        context_label: "ctx 18% · 1200/8000".to_owned(),
    };

    assert_eq!(footer_context_width(&footer, 20), 0);
    assert_eq!(footer_context_width(&footer, 60), 19);
}

#[test]
fn render_main_screen_shows_esc_interrupt_for_running_turn() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle_key_event(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE))?;
    let _ = app.submit_input()?;
    let backend = TestBackend::new(120, 24);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Esc interrupt"));
    assert!(rendered.contains("reasoning with deepseek-v4-flash"));
    Ok(())
}

#[test]
fn render_config_screen_uses_details_side_panel_on_wide_terminals() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let backend = TestBackend::new(160, 36);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("Config"));
    assert!(rendered.contains("Details"));
    assert!(rendered.contains("Provider 1/5"));
    assert!(rendered.contains("focus Model"));
    assert!(rendered.contains("key model"));
    assert!(rendered.contains("keys Tab section"));
    assert!(rendered.contains("actions Down to actions"));
    assert!(rendered.contains("state saved"));
    assert!(!rendered.contains("Status"));
    assert!(!rendered.contains("Actions"));
    assert!(!rendered.contains("provider settings · Tab"));
    assert!(!rendered.contains("[details]"));
    Ok(())
}

#[test]
fn render_config_common_widths_keep_core_structure() -> anyhow::Result<()> {
    for width in [80, 96, 160] {
        for (right_presses, title, selected) in [
            (0, "Provider 1/5", "focus Model"),
            (2, "Memory 3/5", "focus Memory"),
            (3, "Compaction 4/5", "focus Auto compact"),
        ] {
            let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
            app.input = "/config".to_owned();
            let _ = app.submit_input()?;
            for _ in 0..right_presses {
                let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
            }
            let backend = TestBackend::new(width, 28);
            let mut terminal = Terminal::new(backend)?;

            terminal.draw(|frame| render(frame, &app))?;

            let rendered = rendered_content(&terminal);
            assert!(
                rendered.contains("Sigil config"),
                "width {width} should keep the config header for {title}"
            );
            assert!(
                rendered.contains(title),
                "width {width} should keep the section title {title}"
            );
            assert!(
                rendered.contains(selected),
                "width {width} should keep details row {selected}"
            );
            assert!(
                rendered.contains("[save]") || rendered.contains("> save <"),
                "width {width} should keep footer save action for {title}"
            );
        }
    }
    Ok(())
}

#[test]
fn render_config_centers_content_on_very_wide_terminals() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let backend = TestBackend::new(220, 36);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rows = rendered_rows(&terminal);
    let header_x = rows
        .iter()
        .find_map(|row| char_index_of(row, "Sigil config"))
        .expect("config header should render");
    let panel_x = rows
        .iter()
        .filter(|row| !row.contains("Sigil config"))
        .find_map(|row| char_index_of(row, "Config"))
        .expect("config panel should render");
    let footer_x = rows
        .iter()
        .find_map(|row| char_index_of(row, "[save]"))
        .expect("config footer should render");
    let details_x = rows
        .iter()
        .find_map(|row| char_index_of(row, "Details"))
        .expect("details panel should render");

    assert!(header_x >= 20, "header should not start at the screen edge");
    assert!(panel_x >= 20, "panel should not start at the screen edge");
    assert!(footer_x >= 20, "footer should not start at the screen edge");
    assert!(header_x.abs_diff(footer_x) <= 5);
    assert!(details_x > panel_x);
    assert!(
        details_x < 190,
        "content should stay bounded on wide screens: details_x={details_x}"
    );
    Ok(())
}

#[test]
fn render_config_header_uses_segmented_summary() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let backend = TestBackend::new(96, 28);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rows = rendered_rows(&terminal);
    let title_row = rows
        .iter()
        .find(|row| row.contains("Sigil config"))
        .expect("config title row should render");
    let file_row = rows
        .iter()
        .find(|row| row.contains("file sigil.toml"))
        .expect("config file row should render");

    assert!(title_row.contains("Provider"));
    assert!(title_row.contains(" saved "));
    assert!(title_row.contains("field: Model"));
    assert!(!title_row.contains("Provider · saved"));
    assert!(file_row.contains("note opened config"));
    Ok(())
}

#[test]
fn render_config_footer_follows_short_content() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let backend = TestBackend::new(160, 40);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rows = rendered_rows(&terminal);
    let footer_y = rows
        .iter()
        .position(|row| row.contains("[save]"))
        .expect("config footer should render");
    let buffer = terminal.backend().buffer();
    let panel_bottom_y = (0..buffer.area.height)
        .filter(|y| {
            (0..buffer.area.width)
                .filter(|x| buffer.cell((*x, *y)).expect("cell in bounds").fg == config_border())
                .count()
                > 20
        })
        .map(usize::from)
        .max()
        .expect("config panel border should render");

    assert!(footer_y > panel_bottom_y);
    assert!(
        footer_y <= panel_bottom_y + 2,
        "footer should stay near the panel: footer_y={footer_y}, panel_bottom_y={panel_bottom_y}"
    );
    assert!(
        footer_y < rows.len().saturating_sub(8),
        "footer should not stay pinned to the bottom of a tall terminal"
    );
    Ok(())
}

#[test]
fn render_config_footer_uses_toolbar_layout_on_wide_terminals() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let backend = TestBackend::new(160, 36);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rows = rendered_rows(&terminal);
    let footer_y = rows
        .iter()
        .position(|row| row.contains("[save]") && row.contains("state saved"))
        .expect("config footer toolbar should render");
    let footer_row = &rows[footer_y];
    let close_x = char_index_of(footer_row, "[close]").expect("close action should render");
    let status_x = char_index_of(footer_row, "state saved").expect("status should render");
    let chip_bg_cells = (0..terminal.backend().buffer().area.width)
        .filter(|x| {
            terminal
                .backend()
                .buffer()
                .cell((*x, footer_y as u16))
                .expect("cell in bounds")
                .bg
                == config_tab_bg()
        })
        .count();

    assert!(status_x > close_x + 32);
    assert!(footer_row.trim_end().ends_with("state saved"));
    assert!(chip_bg_cells > 30);
    Ok(())
}

#[test]
fn render_config_screen_uses_muted_palette_instead_of_terminal_green() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let backend = TestBackend::new(160, 36);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let cells = terminal.backend().buffer().content();
    assert!(cells.iter().any(|cell| cell.bg == config_primary()));
    assert!(cells.iter().any(|cell| cell.fg == config_border()));
    assert!(
        !cells
            .iter()
            .any(|cell| cell.fg == Color::Green || cell.bg == Color::Green)
    );
    Ok(())
}

#[test]
fn render_config_screen_uses_subtle_sections_and_full_selected_row() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let backend = TestBackend::new(160, 36);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rows = rendered_rows(&terminal);
    let selected_row = rows
        .iter()
        .position(|row| row.contains("Model") && row.contains("deepseek-v4-flash"))
        .expect("selected model field row should render");
    let section_row = rows
        .iter()
        .position(|row| row.contains(" model ") && row.contains("──"))
        .expect("model section label should render");
    let buffer = terminal.backend().buffer();
    let selected_bg_cells = (0..buffer.area.width)
        .filter(|x| {
            buffer
                .cell((*x, selected_row as u16))
                .expect("cell in bounds")
                .bg
                == config_selected_bg()
        })
        .count();
    let section_chip_cells = (0..buffer.area.width)
        .filter(|x| {
            buffer
                .cell((*x, section_row as u16))
                .expect("cell in bounds")
                .bg
                == config_section_bg()
        })
        .count();

    assert!(
        selected_bg_cells > 40,
        "selected row should have a visible focus band, got {selected_bg_cells} selected-bg cells"
    );
    assert!(
        (6..20).contains(&section_chip_cells),
        "section chip should be local, got {section_chip_cells} highlighted cells"
    );
    assert!(rows[section_row].contains("──"));
    Ok(())
}

#[test]
fn render_config_form_rows_align_value_column() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let backend = TestBackend::new(132, 36);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rows = rendered_rows(&terminal);
    let value_columns = ["Model", "API key", "Endpoint", "FIM model"]
        .into_iter()
        .map(|label| {
            rows.iter()
                .find(|row| {
                    row.contains(label)
                        && row.contains(':')
                        && !row.contains("field:")
                        && !row.contains("focus ")
                })
                .and_then(|row| char_index_of(row, ":"))
                .unwrap_or_else(|| panic!("{label} row should render with a colon"))
        })
        .collect::<Vec<_>>();

    assert!(
        value_columns.windows(2).all(|pair| pair[0] == pair[1]),
        "config value columns should align: {value_columns:?}"
    );
    Ok(())
}

#[test]
fn render_config_form_action_chips_align_to_action_column() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let backend = TestBackend::new(132, 36);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;
    let rows = rendered_rows(&terminal);
    let model_action_x = rows
        .iter()
        .find(|row| row.contains("Model") && row.contains("[choose]"))
        .and_then(|row| char_index_of(row, "[choose]"))
        .expect("model action chip should render");
    assert!(
        !rows.iter().any(|row| row.contains("[Enter choose]")),
        "main config form should keep shortcut text out of action chips"
    );

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    terminal.draw(|frame| render(frame, &app))?;
    let rows = rendered_rows(&terminal);
    let endpoint_action_x = rows
        .iter()
        .find(|row| row.contains("Endpoint") && row.contains("[input]"))
        .and_then(|row| char_index_of(row, "[input]"))
        .expect("endpoint action chip should render");
    assert!(
        !rows.iter().any(|row| row.contains("[Enter input]")),
        "main config form should keep shortcut text out of action chips"
    );

    assert_eq!(
        model_action_x, endpoint_action_x,
        "config action chips should share a stable action column"
    );
    Ok(())
}

#[test]
fn render_config_readonly_rows_align_value_column() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    let backend = TestBackend::new(132, 36);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rows = rendered_rows(&terminal);
    let documents_row = rows
        .iter()
        .position(|row| row.contains("read Documents"))
        .expect("documents readonly row should render");
    let value_columns = ["Documents", "Last scan", "Root files"]
        .into_iter()
        .map(|label| {
            rows.iter()
                .find(|row| row.contains(label) && row.contains(':'))
                .and_then(|row| char_index_of(row, ":"))
                .unwrap_or_else(|| panic!("{label} row should render with a colon"))
        })
        .collect::<Vec<_>>();

    assert!(
        value_columns.windows(2).all(|pair| pair[0] == pair[1]),
        "config readonly value columns should align: {value_columns:?}"
    );
    let readonly_chip_cells = (0..terminal.backend().buffer().area.width)
        .filter(|x| {
            terminal
                .backend()
                .buffer()
                .cell((*x, documents_row as u16))
                .expect("cell in bounds")
                .bg
                == config_tab_bg()
        })
        .count();
    assert_eq!(readonly_chip_cells, "read ".chars().count());
    Ok(())
}

#[test]
fn render_config_details_panel_uses_focus_row_and_command_tokens() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let backend = TestBackend::new(160, 36);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rows = rendered_rows(&terminal);
    let selected_detail_row = rows
        .iter()
        .position(|row| row.contains("focus Model"))
        .expect("selected field detail should render");
    let controls_row = rows
        .iter()
        .position(|row| row.contains("keys Tab section"))
        .expect("controls detail should render");
    let key_row = rows
        .iter()
        .position(|row| row.contains("key model"))
        .expect("key metadata detail should render");
    let help_row = rows
        .iter()
        .position(|row| row.contains("i Chat model used for new"))
        .expect("field help should render as an info row");
    let buffer = terminal.backend().buffer();
    let selected_bg_cells = (0..buffer.area.width)
        .filter(|x| {
            buffer
                .cell((*x, selected_detail_row as u16))
                .expect("cell in bounds")
                .bg
                == config_selected_bg()
        })
        .count();
    let command_token_cells = (0..buffer.area.width)
        .filter(|x| {
            buffer
                .cell((*x, controls_row as u16))
                .expect("cell in bounds")
                .bg
                == config_tab_bg()
        })
        .count();

    assert!(selected_bg_cells > 20);
    assert!((15..25).contains(&command_token_cells));
    assert!(rows[key_row].contains("key model"));
    assert!(rows[help_row].contains("..."));
    assert!(
        !rendered_content(&terminal)
            .contains("Switching the saved default does not rewrite the current session")
    );
    Ok(())
}

#[test]
fn render_config_screen_panel_height_tracks_content() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let backend = TestBackend::new(160, 36);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rows = rendered_rows(&terminal);
    let panel_bottom = rows
        .iter()
        .position(|row| row.contains('╰'))
        .expect("config panel should have a bottom border");
    assert!(panel_bottom < 22);
    assert!(!rows[31].contains('│'));
    Ok(())
}

#[test]
fn render_config_text_modal_uses_field_help_and_value_label() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let backend = TestBackend::new(160, 36);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("Endpoint"));
    assert!(rendered.contains("OpenAI-compatible DeepSeek endpoint"));
    assert!(rendered.contains("key base_url"));
    assert!(rendered.contains("value: https://api.deepseek.com"));
    Ok(())
}

#[test]
fn render_config_text_modal_uses_focus_input_row_and_command_tokens() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let backend = TestBackend::new(160, 36);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rows = rendered_rows(&terminal);
    let input_row = rows
        .iter()
        .position(|row| row.contains("value: https://api.deepseek.com|"))
        .expect("text modal input row should render");
    let commands_row = rows
        .iter()
        .position(|row| row.contains("Enter apply"))
        .expect("text modal command row should render");
    let buffer = terminal.backend().buffer();
    let input_bg_cells = (0..buffer.area.width)
        .filter(|x| {
            buffer
                .cell((*x, input_row as u16))
                .expect("cell in bounds")
                .bg
                == config_selected_bg()
        })
        .count();
    let command_token_cells = (0..buffer.area.width)
        .filter(|x| {
            buffer
                .cell((*x, commands_row as u16))
                .expect("cell in bounds")
                .bg
                == config_tab_bg()
        })
        .count();

    assert!(input_bg_cells > 20);
    assert_eq!(command_token_cells, "EnterF2F3Esc".chars().count());
    Ok(())
}

#[test]
fn render_config_model_picker_uses_config_palette() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let backend = TestBackend::new(160, 36);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let cells = terminal.backend().buffer().content();
    assert!(cells.iter().any(|cell| cell.bg == config_selected_bg()));
    assert!(
        !cells
            .iter()
            .any(|cell| matches!(cell.fg, Color::Green | Color::Cyan)
                || matches!(cell.bg, Color::Green | Color::Cyan))
    );
    Ok(())
}

#[test]
fn render_config_model_picker_uses_focus_row_and_command_tokens() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let backend = TestBackend::new(160, 36);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rows = rendered_rows(&terminal);
    let selected_model_row = rows
        .iter()
        .position(|row| row.contains("> deepseek-v4-flash"))
        .expect("selected model picker row should render");
    let commands_row = rows
        .iter()
        .position(|row| row.contains("Up/Down choose"))
        .expect("model picker command row should render");
    let buffer = terminal.backend().buffer();
    let selected_bg_cells = (0..buffer.area.width)
        .filter(|x| {
            buffer
                .cell((*x, selected_model_row as u16))
                .expect("cell in bounds")
                .bg
                == config_selected_bg()
        })
        .count();
    let command_token_cells = (0..buffer.area.width)
        .filter(|x| {
            buffer
                .cell((*x, commands_row as u16))
                .expect("cell in bounds")
                .bg
                == config_tab_bg()
        })
        .count();

    assert!(selected_bg_cells > 20);
    assert_eq!(command_token_cells, "Up/DownEnterF2F3Esc".chars().count());
    Ok(())
}

#[test]
fn render_config_screen_keeps_single_panel_on_narrow_terminals() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let backend = TestBackend::new(96, 28);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("Config"));
    assert!(rendered.contains("details"));
    assert!(!rendered.contains("Details"));
    assert!(rendered.contains("focus Model"));
    Ok(())
}

#[test]
fn render_config_header_truncates_long_status_summary() -> anyhow::Result<()> {
    let long_config_name = "sigil-config-file-name-with-a-very-very-long-project-suffix.toml";
    let mut app = AppState::from_root_config(Path::new(long_config_name), &test_config());
    app.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let backend = TestBackend::new(80, 28);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("Sigil config"));
    assert!(rendered.contains("field: Model"));
    assert!(rendered.contains("..."));
    assert!(!rendered.contains(long_config_name));
    Ok(())
}

#[test]
fn render_config_narrow_screen_keeps_details_visual_hierarchy() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let backend = TestBackend::new(48, 28);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rows = rendered_rows(&terminal);
    let selected_detail_row = rows
        .iter()
        .position(|row| row.contains("focus Model"))
        .expect("narrow selected detail should render");
    let controls_row = rows
        .iter()
        .position(|row| row.contains("keys Tab section"))
        .expect("narrow controls detail should render");
    let buffer = terminal.backend().buffer();
    let selected_bg_cells = (0..buffer.area.width)
        .filter(|x| {
            buffer
                .cell((*x, selected_detail_row as u16))
                .expect("cell in bounds")
                .bg
                == config_selected_bg()
        })
        .count();
    let command_token_cells = (0..buffer.area.width)
        .filter(|x| {
            buffer
                .cell((*x, controls_row as u16))
                .expect("cell in bounds")
                .bg
                == config_tab_bg()
        })
        .count();

    assert!(selected_bg_cells > 20);
    assert!((12..24).contains(&command_token_cells));
    assert!(rows[controls_row].contains("Enter edit"));
    Ok(())
}

#[test]
fn render_config_narrow_screen_truncates_long_values() -> anyhow::Result<()> {
    let mut config = test_config();
    let long_model =
        "deepseek-v4-pro-with-a-very-very-long-model-name-that-should-truncate".to_owned();
    config.agent.model = long_model.clone();
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let backend = TestBackend::new(96, 28);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("deepseek-v4-pro"));
    assert!(rendered.contains("..."));
    assert!(!rendered.contains(&long_model));
    assert!(app.config_detail_lines().join("\n").contains(&long_model));
    Ok(())
}

#[test]
fn render_config_short_terminal_scrolls_to_selected_field() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/config".to_owned();
    let _ = app.submit_input()?;
    for _ in 0..3 {
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    }
    for _ in 0..4 {
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    }
    let backend = TestBackend::new(96, 12);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("> Tail messages"));
    assert!(rendered.contains("more ^"));
    assert!(rendered.contains("more v"));
    let rows = rendered_rows(&terminal);
    let selected_row = rows
        .iter()
        .position(|row| row.contains("> Tail messages"))
        .expect("selected tail messages row should render");
    let buffer = terminal.backend().buffer();
    let selected_bg_cells = (0..buffer.area.width)
        .filter(|x| {
            buffer
                .cell((*x, selected_row as u16))
                .expect("cell in bounds")
                .bg
                == config_selected_bg()
        })
        .count();

    assert!(selected_bg_cells > 20);
    Ok(())
}

#[test]
fn render_config_footer_tracks_dirty_and_confirm_close_states() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let backend = TestBackend::new(132, 30);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("state unsaved - save before close"));
    assert!(rendered.contains("[save]"));
    assert!(rendered.contains("[save+close]"));
    assert!(rendered.contains("[close]"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    terminal.draw(|frame| render(frame, &app))?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("state confirm close - Esc discards"));
    assert!(rendered.contains("> save <"));
    Ok(())
}

#[test]
fn render_config_footer_compacts_on_narrow_terminals() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    let backend = TestBackend::new(64, 24);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("> save <"));
    assert!(rendered.contains("[save+close]"));
    assert!(rendered.contains("[close]"));
    assert!(rendered.contains("..."));
    assert!(!rendered.contains("state confirm close - Esc discards"));
    Ok(())
}

#[test]
fn render_config_screen_marks_readonly_and_hint_rows() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    let backend = TestBackend::new(132, 30);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("Memory 3/5"));
    assert!(rendered.contains("read Documents"));
    assert!(rendered.contains("read Last scan"));
    assert!(rendered.contains("read Root files"));
    Ok(())
}

fn rendered_content(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>()
}

fn rendered_rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer.cell((x, y)).expect("cell in bounds").symbol())
                .collect::<String>()
        })
        .collect()
}

fn char_index_of(row: &str, needle: &str) -> Option<usize> {
    row.find(needle)
        .map(|byte_index| row[..byte_index].chars().count())
}
