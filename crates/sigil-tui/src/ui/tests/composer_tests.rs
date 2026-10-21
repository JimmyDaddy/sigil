use ratatui::{Terminal, backend::TestBackend};

use crate::{
    timeline::{RunPhase, SidebarAgentRow},
    ui::theme::Theme,
};

use super::*;

#[test]
fn composer_header_prioritizes_mode_and_permission_on_narrow_screen() -> anyhow::Result<()> {
    let view_model = ComposerViewModel {
        mode_label: "Build".to_owned(),
        permission_label: "manual".to_owned(),
        phase: RunPhase::Idle,
        model_name: "very-long-model-name-that-must-not-hide-permission".to_owned(),
        agent_rows: Vec::new(),
        agent_panel_focused: false,
        image_attachments: Vec::new(),
        input: "你好".to_owned(),
        input_rows: 1,
        cursor_position: (4, 0),
        selection: None,
    };
    let mut terminal = Terminal::new(TestBackend::new(32, 5))?;
    terminal.draw(|frame| render_input(frame, frame.area(), &view_model))?;
    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Build · manual"));
    assert!(!rendered.contains("agent: main"));
    assert_eq!(
        terminal
            .backend()
            .buffer()
            .cell((3, 3))
            .expect("input cell")
            .symbol(),
        "你"
    );
    assert_eq!(
        terminal
            .backend()
            .buffer()
            .cell((5, 3))
            .expect("input cell")
            .symbol(),
        "好"
    );
    assert_eq!(
        composer_cursor_position(Rect::new(0, 0, 32, 5), &view_model),
        Some((7, 3))
    );
    Ok(())
}

#[test]
fn composer_highlights_known_slash_command_token() -> anyhow::Result<()> {
    let view_model = ComposerViewModel {
        mode_label: "Build".to_owned(),
        phase: RunPhase::Idle,
        permission_label: "manual".to_owned(),
        model_name: "deepseek-v4-pro".to_owned(),
        agent_rows: Vec::new(),
        agent_panel_focused: false,
        image_attachments: Vec::new(),
        input: "/task fix typo".to_owned(),
        input_rows: 1,
        cursor_position: (14, 0),
        selection: None,
    };
    let backend = TestBackend::new(72, 5);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_input(frame, frame.area(), &view_model))?;

    let content = terminal.backend().buffer().content();
    let width = 72usize;
    let command_cell = &content[(3 * width) + 3];
    let rest_cell = &content[(3 * width) + 9];
    let theme = Theme::default();
    assert_eq!(command_cell.symbol(), "/");
    assert_eq!(command_cell.fg, theme.palette.accent_info);
    assert!(command_cell.modifier.contains(Modifier::BOLD));
    assert_eq!(rest_cell.symbol(), "f");
    assert_eq!(rest_cell.fg, theme.palette.text_primary);
    Ok(())
}

#[test]
fn composer_input_row_highlights_slash_command_and_pads_tail() {
    let theme = Theme::default();
    let line = render_input_row("/task fix", 20, theme.palette.surface_input, &theme);

    assert_eq!(line.spans[0].content.as_ref(), "");
    assert_eq!(line.spans[1].content.as_ref(), "/task");
    assert_eq!(line.spans[1].style.fg, Some(theme.palette.accent_info));
    assert!(line.spans[1].style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(line.spans[2].content.as_ref(), " fix           ");

    let indented = render_input_row("  /task fix", 14, theme.palette.surface_input, &theme);
    assert_eq!(indented.spans[0].content.as_ref(), "  ");
    assert_eq!(indented.spans[1].content.as_ref(), "/task");
    assert_eq!(indented.spans[2].content.as_ref(), " fix   ");
}

#[test]
fn composer_input_row_sanitizes_controls_before_slash_token_offsets() {
    let theme = Theme::default();
    let line = render_input_row("\t/task", 5, theme.palette.surface_input, &theme);

    assert_eq!(line.spans[0].content.as_ref(), "");
    assert_eq!(line.spans[1].content.as_ref(), "/task");
    assert_eq!(line.spans[2].content.as_ref(), "");
    assert_eq!(terminal_cell_width("/task"), 5);
}

#[test]
fn composer_input_aligns_with_header_after_gap() -> anyhow::Result<()> {
    let view_model = ComposerViewModel {
        mode_label: "Build".to_owned(),
        phase: RunPhase::Idle,
        permission_label: "manual".to_owned(),
        model_name: "deepseek-v4-pro".to_owned(),
        agent_rows: Vec::new(),
        agent_panel_focused: false,
        image_attachments: Vec::new(),
        input: "/resume".to_owned(),
        input_rows: 1,
        cursor_position: (7, 0),
        selection: None,
    };
    let backend = TestBackend::new(72, 5);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_input(frame, frame.area(), &view_model))?;

    let content = terminal.backend().buffer().content();
    let width = 72usize;
    let rendered = content.iter().map(|cell| cell.symbol()).collect::<String>();
    assert!(rendered.contains("Build"));
    assert!(rendered.contains("deepseek-v4-pro"));
    assert!(rendered.contains("deepseek"));
    assert!(rendered.contains("manual"));
    assert!(!rendered.contains(" ·  deepseek"));
    assert!(!rendered.contains("max"));
    assert!(!rendered.contains("Composer"));
    assert_eq!(content[0].symbol(), " ");
    assert_eq!(content[1].symbol(), "▌");
    assert_eq!(content[3].symbol(), "─");
    assert_eq!(content[width + 3].symbol(), "B");
    assert_eq!(content[(2 * width) + 3].symbol(), " ");
    assert_eq!(content[(3 * width) + 3].symbol(), "/");
    Ok(())
}

#[test]
fn composer_renders_selected_attachment_above_the_input() -> anyhow::Result<()> {
    let view_model = ComposerViewModel {
        mode_label: "Build".to_owned(),
        phase: RunPhase::Idle,
        permission_label: "manual".to_owned(),
        model_name: "gpt-5".to_owned(),
        agent_rows: Vec::new(),
        agent_panel_focused: false,
        image_attachments: vec![crate::view_model::ComposerAttachmentViewModel {
            label: "image 1 · PNG · 640×480 · 12.0 KiB".to_owned(),
            selected: true,
        }],
        input: "describe it".to_owned(),
        input_rows: 1,
        cursor_position: (11, 0),
        selection: None,
    };
    let backend = TestBackend::new(88, 6);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_input(frame, frame.area(), &view_model))?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("◆ image 1 · PNG · 640×480 · 12.0 KiB"));
    assert!(rendered.contains("Backspace/Delete remove"));
    assert_eq!(
        composer_cursor_origin(ratatui::layout::Rect::new(0, 0, 88, 6), &view_model),
        Some((3, 4))
    );
    Ok(())
}

#[test]
fn compact_composer_windows_attachments_around_selection_and_keeps_input_visible()
-> anyhow::Result<()> {
    let view_model = ComposerViewModel {
        mode_label: "Build".to_owned(),
        phase: RunPhase::Idle,
        permission_label: "manual".to_owned(),
        model_name: "gpt-5".to_owned(),
        agent_rows: Vec::new(),
        agent_panel_focused: false,
        image_attachments: (0..4)
            .map(|index| crate::view_model::ComposerAttachmentViewModel {
                label: format!("image {} · PNG · 1×1 · 4 B", index + 1),
                selected: index == 3,
            })
            .collect(),
        input: "draft remains editable".to_owned(),
        input_rows: 1,
        cursor_position: (5, 0),
        selection: None,
    };
    let area = ratatui::layout::Rect::new(0, 0, 40, 6);
    let backend = TestBackend::new(40, 6);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_input(frame, area, &view_model))?;

    let rows = (0..6)
        .map(|y| {
            (0..40)
                .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert!(rows.iter().any(|row| row.contains("… ◆ image 4")));
    assert!(!rows.iter().any(|row| row.contains("image 1")));
    assert!(rows.iter().any(|row| row.contains("draft remains")));
    let input_area = composer_input_area(area, 1, 4);
    assert_eq!(input_area.height, 1);
    assert_eq!(composer_cursor_origin(area, &view_model), Some((3, 4)));
    assert_eq!(
        composer_attachment_window(area, 4, 3),
        ComposerAttachmentWindow {
            item_indices: vec![3],
            show_overflow_row: false,
            inline_overflow: true,
        }
    );
    Ok(())
}

#[test]
fn agent_panel_line_pads_tail_with_panel_background() {
    let theme = Theme::default();
    let bg = theme.palette.surface_agent_panel;
    let line = agent_panel_line(Line::from(vec![Span::raw("main")]), 10, bg);

    assert_eq!(spans_display_width(&line.spans), 10);
    assert_eq!(line.style.bg, Some(bg));
    assert_eq!(line.spans[0].style.bg, Some(bg));
    let tail = line.spans.last().expect("expected padded tail span");
    assert_eq!(tail.content.as_ref(), "      ");
    assert_eq!(tail.style.bg, Some(bg));
}

#[test]
fn composer_cursor_origin_scrolls_with_multiline_input() {
    let view_model = ComposerViewModel {
        mode_label: "Build".to_owned(),
        phase: RunPhase::Idle,
        permission_label: "manual".to_owned(),
        model_name: "deepseek-v4-pro".to_owned(),
        agent_rows: Vec::new(),
        agent_panel_focused: false,
        image_attachments: Vec::new(),
        input: "one\ntwo\nthree\nfour".to_owned(),
        input_rows: 4,
        cursor_position: (2, 3),
        selection: None,
    };

    assert_eq!(
        composer_cursor_origin(ratatui::layout::Rect::new(0, 0, 32, 5), &view_model),
        Some((3, 3))
    );
}

#[test]
fn composer_cursor_origin_returns_none_when_input_area_disappears() {
    let view_model = ComposerViewModel {
        mode_label: "Build".to_owned(),
        phase: RunPhase::Idle,
        permission_label: "manual".to_owned(),
        model_name: "deepseek-v4-pro".to_owned(),
        agent_rows: Vec::new(),
        agent_panel_focused: false,
        image_attachments: Vec::new(),
        input: String::new(),
        input_rows: 1,
        cursor_position: (0, 0),
        selection: None,
    };

    assert_eq!(
        composer_cursor_origin(ratatui::layout::Rect::new(0, 0, 4, 2), &view_model),
        None
    );
}

#[test]
fn composer_cursor_position_clamps_to_the_visible_input_width() {
    let view_model = ComposerViewModel {
        mode_label: "Build".to_owned(),
        phase: RunPhase::Idle,
        permission_label: "manual".to_owned(),
        model_name: "deepseek-v4-pro".to_owned(),
        agent_rows: Vec::new(),
        agent_panel_focused: false,
        image_attachments: Vec::new(),
        input: "abcdefgh".to_owned(),
        input_rows: 8,
        cursor_position: (8, 7),
        selection: None,
    };

    assert_eq!(
        composer_cursor_position(ratatui::layout::Rect::new(0, 0, 7, 5), &view_model),
        Some((3, 3))
    );
}

#[test]
fn render_input_with_insufficient_height_skips_render_and_keeps_empty_area() -> anyhow::Result<()> {
    let view_model = ComposerViewModel {
        mode_label: "Build".to_owned(),
        phase: RunPhase::Idle,
        permission_label: "manual".to_owned(),
        model_name: "deepseek-v4-pro".to_owned(),
        agent_rows: Vec::new(),
        agent_panel_focused: false,
        image_attachments: Vec::new(),
        input: "hello".to_owned(),
        input_rows: 1,
        cursor_position: (0, 0),
        selection: None,
    };
    let backend = TestBackend::new(20, 1);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_input(frame, frame.area(), &view_model))?;
    let symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell: &ratatui::buffer::Cell| cell.symbol())
        .collect::<String>();
    assert!(!symbols.contains("Build"));

    Ok(())
}

#[test]
fn render_input_pads_unused_input_rows_with_input_background() -> anyhow::Result<()> {
    let view_model = ComposerViewModel {
        mode_label: "Build".to_owned(),
        phase: RunPhase::Idle,
        permission_label: "manual".to_owned(),
        model_name: "deepseek-v4-pro".to_owned(),
        agent_rows: Vec::new(),
        agent_panel_focused: false,
        image_attachments: Vec::new(),
        input: "one".to_owned(),
        input_rows: 1,
        cursor_position: (3, 0),
        selection: None,
    };
    let backend = TestBackend::new(32, 6);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_input(frame, frame.area(), &view_model))?;

    let content = terminal.backend().buffer().content();
    let width = 32usize;
    assert_eq!(content[(4 * width) + 3].symbol(), " ");
    assert_eq!(content[(5 * width) + 3].symbol(), " ");
    Ok(())
}

#[test]
fn render_composer_gutter_with_zero_height_does_not_panic() -> anyhow::Result<()> {
    use ratatui::style::Color;

    let backend = TestBackend::new(1, 1);
    let mut terminal = Terminal::new(backend)?;

    let zero_height_area = ratatui::layout::Rect::new(0, 0, 1, 0);
    let full_area = ratatui::layout::Rect::new(0, 0, 1, 1);
    terminal.draw(|frame| {
        render_composer_gutter(frame, zero_height_area, Color::Red, Color::Black);
        render_input(
            frame,
            full_area,
            &ComposerViewModel {
                mode_label: "Build".to_owned(),
                phase: RunPhase::Idle,
                permission_label: "manual".to_owned(),
                model_name: "deepseek-v4-pro".to_owned(),
                agent_rows: Vec::new(),
                agent_panel_focused: false,
                image_attachments: Vec::new(),
                input: "".to_owned(),
                input_rows: 1,
                cursor_position: (0, 0),
                selection: None,
            },
        );
    })?;

    Ok(())
}

#[test]
fn composer_helpers_cover_zero_width_and_agent_row_styles() -> anyhow::Result<()> {
    use ratatui::{layout::Rect, style::Color};

    let view_model = ComposerViewModel {
        mode_label: "Build".to_owned(),
        phase: RunPhase::Idle,
        permission_label: "manual".to_owned(),
        model_name: "deepseek-v4-pro".to_owned(),
        agent_rows: vec![SidebarAgentRow {
            label: "main".to_owned(),
            detail: "idle in current session".to_owned(),
            selected: true,
            active: true,
            muted: false,
        }],
        agent_panel_focused: true,
        image_attachments: Vec::new(),
        input: String::new(),
        input_rows: 1,
        cursor_position: (0, 0),
        selection: None,
    };
    let backend = TestBackend::new(8, 2);
    let mut terminal = Terminal::new(backend)?;
    let theme = Theme::default();

    assert_eq!(
        composer_input_area(Rect::new(0, 0, 2, 5), 1, 0),
        Rect::default()
    );
    assert_eq!(panel_separator_area(Rect::new(4, 2, 3, 5)), None);
    assert_eq!(
        panel_separator_area(Rect::new(4, 2, 12, 5)),
        Some(Rect::new(7, 2, 9, 1))
    );
    let muted = SidebarAgentRow {
        label: "quiet reviewer".to_owned(),
        detail: "completed · subagent_read · v1:step_1".to_owned(),
        selected: false,
        active: false,
        muted: true,
    };
    let muted_line = render_agent_row(&muted, 18, false, &theme);
    let muted_text = muted_line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(muted_text.contains("quiet"));

    let selected = SidebarAgentRow {
        selected: true,
        ..muted
    };
    let selected_line = render_agent_row(&selected, 10, true, &theme);
    assert_eq!(
        selected_line.spans.first().and_then(|span| span.style.bg),
        Some(theme.palette.selection_bg)
    );

    terminal.draw(|frame| {
        render_panel_separator(frame, Rect::new(0, 0, 0, 1), Color::Black, Color::White);
        render_agent_panel(frame, Rect::new(0, 0, 0, 0), &view_model);
        render_agent_panel(frame, Rect::new(0, 0, 8, 1), &view_model);
    })?;
    Ok(())
}

#[test]
fn render_agent_panel_shows_agent_rows() -> anyhow::Result<()> {
    let view_model = ComposerViewModel {
        mode_label: "Build · agent: repo audit".to_owned(),
        phase: RunPhase::Idle,
        permission_label: "manual".to_owned(),
        model_name: "deepseek-v4-pro".to_owned(),
        agent_rows: vec![
            SidebarAgentRow {
                label: "main".to_owned(),
                detail: "idle in current session".to_owned(),
                selected: false,
                active: false,
                muted: false,
            },
            SidebarAgentRow {
                label: "repo audit".to_owned(),
                detail: "completed · subagent_read · v1:step_1".to_owned(),
                selected: true,
                active: true,
                muted: false,
            },
        ],
        agent_panel_focused: false,
        image_attachments: Vec::new(),
        input: String::new(),
        input_rows: 1,
        cursor_position: (0, 0),
        selection: None,
    };
    let backend = TestBackend::new(96, 8);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_agent_panel(frame, frame.area(), &view_model))?;

    let rendered =
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .fold(String::new(), |mut output, cell| {
                output.push_str(cell.symbol());
                output
            });
    assert!(!rendered.contains("Agent"));
    assert!(!rendered.contains("Down focus"));
    assert!(!rendered.contains("Alt-A next"));
    assert!(rendered.contains("main"));
    assert!(rendered.contains("repo audit"));
    assert!(rendered.contains("◉ repo audit"));
    assert!(rendered.contains("✓ subagent_read"));
    assert!(!rendered.contains("focused"));
    assert!(!rendered.contains("status:"));
    Ok(())
}

#[test]
fn render_agent_panel_omits_queue_rows_from_agent_surface() -> anyhow::Result<()> {
    let view_model = ComposerViewModel {
        mode_label: "Build · agent: main".to_owned(),
        phase: RunPhase::Idle,
        permission_label: "manual".to_owned(),
        model_name: "deepseek-v4-pro".to_owned(),
        agent_rows: vec![
            SidebarAgentRow {
                label: "main".to_owned(),
                detail: "idle in current session".to_owned(),
                selected: false,
                active: true,
                muted: false,
            },
            SidebarAgentRow {
                label: "explore".to_owned(),
                detail: "running · explore · chat".to_owned(),
                selected: false,
                active: false,
                muted: false,
            },
        ],
        agent_panel_focused: false,
        image_attachments: Vec::new(),
        input: String::new(),
        input_rows: 1,
        cursor_position: (0, 0),
        selection: None,
    };
    let backend = TestBackend::new(96, 8);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_agent_panel(frame, frame.area(), &view_model))?;

    let rendered =
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .fold(String::new(), |mut output, cell| {
                output.push_str(cell.symbol());
                output
            });
    assert!(!rendered.contains("Queue"));
    assert!(!rendered.contains("queued prompt"));
    assert!(!rendered.contains("queued · chat"));
    assert!(rendered.contains("main"));
    Ok(())
}

#[test]
fn render_agent_panel_shows_focused_controls() -> anyhow::Result<()> {
    let view_model = ComposerViewModel {
        mode_label: "Build · agent: main".to_owned(),
        phase: RunPhase::Idle,
        permission_label: "manual".to_owned(),
        model_name: "deepseek-v4-pro".to_owned(),
        agent_rows: vec![
            SidebarAgentRow {
                label: "main".to_owned(),
                detail: "idle in current session".to_owned(),
                selected: false,
                active: true,
                muted: false,
            },
            SidebarAgentRow {
                label: "repo audit".to_owned(),
                detail: "completed · subagent_read · v1:step_1".to_owned(),
                selected: true,
                active: false,
                muted: false,
            },
        ],
        agent_panel_focused: true,
        image_attachments: Vec::new(),
        input: String::new(),
        input_rows: 1,
        cursor_position: (0, 0),
        selection: None,
    };
    let backend = TestBackend::new(96, 8);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_agent_panel(frame, frame.area(), &view_model))?;

    let rendered =
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .fold(String::new(), |mut output, cell| {
                output.push_str(cell.symbol());
                output
            });
    assert!(rendered.contains("Actions"));
    assert!(rendered.contains("Enter switch"));
    assert!(rendered.contains("Alt-C close"));
    assert!(rendered.contains("Alt-M message"));
    assert!(rendered.contains("Esc input"));
    assert!(rendered.contains("▸ repo audit"));
    assert!(rendered.contains("◉ main"));
    let theme = Theme::default();
    let buffer = terminal.backend().buffer();
    let selected_row_tail = &buffer[(95, 2)];
    assert_eq!(
        selected_row_tail.style().bg,
        Some(theme.palette.selection_bg)
    );
    Ok(())
}

#[test]
fn render_agent_panel_keeps_each_logical_row_single_line_at_24_columns() -> anyhow::Result<()> {
    let view_model = ComposerViewModel {
        mode_label: "Build · agent: repo audit worker".to_owned(),
        phase: RunPhase::Idle,
        permission_label: "manual".to_owned(),
        model_name: "deepseek-v4-pro".to_owned(),
        agent_rows: vec![
            SidebarAgentRow {
                label: "main".to_owned(),
                detail: "idle in current session".to_owned(),
                selected: false,
                active: true,
                muted: false,
            },
            SidebarAgentRow {
                label: "repo audit worker".to_owned(),
                detail: "completed · subagent_read · v1:step_1".to_owned(),
                selected: true,
                active: false,
                muted: false,
            },
        ],
        agent_panel_focused: true,
        image_attachments: Vec::new(),
        input: String::new(),
        input_rows: 1,
        cursor_position: (0, 0),
        selection: None,
    };
    let backend = TestBackend::new(24, 4);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_agent_panel(frame, frame.area(), &view_model))?;

    let buffer = terminal.backend().buffer();
    let rows = buffer
        .content()
        .chunks(24)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>();
    assert!(rows[1].contains("main"));
    assert!(rows[2].contains("repo audit"));
    assert!(rows[3].contains("Actions"));
    assert!(rows[3].contains("Enter"));
    assert_eq!(
        buffer[(23, 2)].style().bg,
        Some(Theme::default().palette.selection_bg),
        "selected agent row must occupy and style exactly one full content row"
    );
    Ok(())
}

#[test]
fn compact_agent_panel_windows_late_selection_and_keeps_actions_visible() -> anyhow::Result<()> {
    let view_model = ComposerViewModel {
        mode_label: "Build · agent: child 4".to_owned(),
        phase: RunPhase::Idle,
        permission_label: "manual".to_owned(),
        model_name: "deepseek-v4-pro".to_owned(),
        agent_rows: (0..5)
            .map(|index| SidebarAgentRow {
                label: if index == 0 {
                    "main".to_owned()
                } else {
                    format!("child {index}")
                },
                detail: "completed · explorer".to_owned(),
                selected: index == 4,
                active: index == 0,
                muted: false,
            })
            .collect(),
        agent_panel_focused: true,
        image_attachments: Vec::new(),
        input: String::new(),
        input_rows: 1,
        cursor_position: (0, 0),
        selection: None,
    };
    let backend = TestBackend::new(80, 3);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_agent_panel(frame, frame.area(), &view_model))?;

    let rows = terminal
        .backend()
        .buffer()
        .content()
        .chunks(80)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>();
    assert!(rows[1].contains("child 4"));
    assert!(!rows.iter().any(|row| row.contains("main")));
    assert!(rows[2].contains("Enter switch"));
    assert!(rows[2].contains("+4 hidden"));
    assert!(rows[2].contains("Alt-C close"));
    assert_eq!(
        composer_agent_window(&view_model.agent_rows, 2, true),
        ComposerAgentWindow {
            item_indices: vec![4],
            show_actions: true,
            hidden_count: 4,
        }
    );
    Ok(())
}

fn composer_selection_view_model(
    input: &str,
    selection: std::ops::Range<usize>,
    width: usize,
) -> ComposerViewModel {
    let (row, column) =
        crate::ui::visual_position_for_char_cursor(input, input.chars().count(), width);
    ComposerViewModel {
        mode_label: "Build".to_owned(),
        phase: RunPhase::Idle,
        permission_label: "manual".to_owned(),
        model_name: "test-model".to_owned(),
        agent_rows: Vec::new(),
        agent_panel_focused: false,
        image_attachments: Vec::new(),
        input: input.to_owned(),
        input_rows: row as u16 + 1,
        cursor_position: (column as u16, row as u16),
        selection: Some(selection),
    }
}

// Ratatui draws a whole wide grapheme from its leading cell and skips hidden
// continuation cells in the backend diff. Verify the emitted occupied columns.
fn composer_drawn_background_at(
    buffer: &ratatui::buffer::Buffer,
    column: u16,
    row: u16,
) -> Option<ratatui::style::Color> {
    let blank = ratatui::buffer::Buffer::empty(buffer.area);
    let mut background = None;
    for (x, y, cell) in blank.diff_iter(buffer) {
        let width = terminal_cell_width(cell.symbol()).max(1) as u16;
        if y == row && x <= column && column < x.saturating_add(width) {
            background = Some(cell.bg);
        }
    }
    background
}

#[test]
fn composer_selection_highlight_follows_wide_grapheme_wrap_without_selecting_padding()
-> anyhow::Result<()> {
    let mut view_model = composer_selection_view_model("ab你Z", 2..3, 3);
    view_model.cursor_position = (2, 1);
    let mut terminal = Terminal::new(TestBackend::new(9, 6))?;
    terminal.draw(|frame| render_input(frame, frame.area(), &view_model))?;
    let buffer = terminal.backend().buffer();
    let palette = Theme::default().palette;
    assert_eq!(buffer[(3, 3)].symbol(), "a");
    assert_eq!(buffer[(5, 3)].symbol(), " ");
    assert_eq!(buffer[(5, 3)].bg, palette.surface_input);
    assert_eq!(buffer[(3, 4)].symbol(), "你");
    assert_eq!(buffer[(3, 4)].bg, palette.selection_bg);
    assert_eq!(terminal_cell_width(buffer[(3, 4)].symbol()), 2);
    for column in 3..5 {
        assert_eq!(
            composer_drawn_background_at(buffer, column, 4),
            Some(palette.selection_bg)
        );
    }
    assert_eq!(buffer[(5, 4)].symbol(), "Z");
    assert_eq!(buffer[(5, 4)].bg, palette.surface_input);

    view_model.selection = None;
    terminal.draw(|frame| render_input(frame, frame.area(), &view_model))?;
    let buffer = terminal.backend().buffer();
    for column in 3..5 {
        assert_eq!(
            composer_drawn_background_at(buffer, column, 4),
            Some(palette.surface_input)
        );
    }
    assert_eq!(buffer[(5, 3)].bg, palette.surface_input);
    assert_eq!(buffer[(5, 4)].symbol(), "Z");
    Ok(())
}

#[test]
fn composer_selection_80_by_24_viewport_preserves_source_offsets_and_cursor() -> anyhow::Result<()>
{
    let mut view_model = composer_selection_view_model("first\nmiddle\n甲🙂tail", 14..16, 74);
    let area = Rect::new(0, 19, 80, 5);
    let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
    terminal.draw(|frame| render_input(frame, area, &view_model))?;
    let input = composer_input_area(area, view_model.input_rows, 0);
    assert_eq!(input.height, 1);
    let buffer = terminal.backend().buffer();
    let palette = Theme::default().palette;
    assert_eq!(buffer[(input.x, input.y)].symbol(), "甲");
    assert_eq!(buffer[(input.x, input.y)].bg, palette.surface_input);
    assert_eq!(buffer[(input.x + 2, input.y)].symbol(), "🙂");
    assert_eq!(buffer[(input.x + 2, input.y)].bg, palette.selection_bg);
    assert_eq!(
        terminal_cell_width(buffer[(input.x + 2, input.y)].symbol()),
        2
    );
    for column in input.x + 2..input.x + 4 {
        assert_eq!(
            composer_drawn_background_at(buffer, column, input.y),
            Some(palette.selection_bg)
        );
    }
    assert_eq!(buffer[(input.x + 4, input.y)].symbol(), "t");
    assert_eq!(buffer[(input.x + 4, input.y)].bg, palette.selection_bg);
    assert_eq!(buffer[(input.x + 5, input.y)].symbol(), "a");
    assert_eq!(buffer[(input.x + 5, input.y)].bg, palette.surface_input);
    assert_eq!(buffer[(input.x + 8, input.y)].bg, palette.surface_input);
    assert_eq!(
        composer_cursor_position(area, &view_model),
        Some((input.x + 8, input.y))
    );
    assert_ne!(buffer[(3, 20)].bg, palette.selection_bg);

    view_model.selection = None;
    terminal.draw(|frame| render_input(frame, area, &view_model))?;
    let buffer = terminal.backend().buffer();
    for column in input.x + 2..input.x + 5 {
        assert_eq!(
            composer_drawn_background_at(buffer, column, input.y),
            Some(palette.surface_input)
        );
    }
    assert_eq!(buffer[(input.x, input.y)].symbol(), "甲");
    assert_eq!(buffer[(input.x + 5, input.y)].symbol(), "a");
    assert_eq!(
        composer_cursor_position(area, &view_model),
        Some((input.x + 8, input.y))
    );
    Ok(())
}

#[test]
fn composer_selection_overlay_preserves_slash_emphasis_and_whole_graphemes() {
    let theme = Theme::default();
    let rows = wrap_composer_input("/task fix", 20);
    let line = render_selected_input_row(
        &rows[0],
        Some(&(1..3)),
        20,
        theme.palette.surface_input,
        &theme,
    );
    assert_eq!(line.spans[0].content.as_ref(), "/");
    assert_eq!(line.spans[0].style.fg, Some(theme.palette.accent_info));
    assert_eq!(line.spans[1].content.as_ref(), "t");
    assert_eq!(line.spans[1].style.bg, Some(theme.palette.selection_bg));
    assert!(line.spans[1].style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(line.spans[3].content.as_ref(), "s");
    assert_eq!(line.spans[3].style.fg, Some(theme.palette.accent_info));
    assert_eq!(line.spans[3].style.bg, Some(theme.palette.surface_input));

    let rows = wrap_composer_input("e\u{301}👨‍👩‍👧‍👦ｶﾞ", 10);
    let line = render_selected_input_row(
        &rows[0],
        Some(&(1..3)),
        10,
        theme.palette.surface_input,
        &theme,
    );
    assert_eq!(line.spans[0].content.as_ref(), "e\u{301}");
    assert_eq!(line.spans[0].style.bg, Some(theme.palette.selection_bg));
    assert_eq!(line.spans[1].content.as_ref(), "👨‍👩‍👧‍👦");
    assert_eq!(line.spans[1].style.bg, Some(theme.palette.selection_bg));
    assert_eq!(line.spans[2].content.as_ref(), "ｶﾞ");
    assert_eq!(line.spans[2].style.bg, Some(theme.palette.surface_input));
}

#[test]
fn composer_selection_hard_newline_marks_one_cell_without_selecting_rest_of_padding() {
    let theme = Theme::default();
    let rows = wrap_composer_input("a\n\nbc\n", 10);
    let selection = 1..5;
    assert_eq!(rows.len(), 4);
    for row in rows.iter().take(3) {
        let line = render_selected_input_row(
            row,
            Some(&selection),
            10,
            theme.palette.surface_input,
            &theme,
        );
        let visible = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(terminal_cell_width(&visible), 10);
        let padding = line.spans.last().expect("unselected trailing padding");
        assert_eq!(padding.style.bg, Some(theme.palette.surface_input));
    }
    let blank_line = render_selected_input_row(
        &rows[1],
        Some(&selection),
        10,
        theme.palette.surface_input,
        &theme,
    );
    assert_eq!(blank_line.spans[0].content.as_ref(), " ");
    assert_eq!(
        blank_line.spans[0].style.bg,
        Some(theme.palette.selection_bg)
    );
    assert_eq!(blank_line.spans[1].content.as_ref(), "         ");
    assert_eq!(
        blank_line.spans[1].style.bg,
        Some(theme.palette.surface_input)
    );
    let tail = render_selected_input_row(
        &rows[3],
        Some(&selection),
        10,
        theme.palette.surface_input,
        &theme,
    );
    assert!(
        tail.spans
            .iter()
            .all(|span| span.style.bg == Some(theme.palette.surface_input))
    );
}

#[test]
fn composer_selection_exact_width_newline_has_no_extra_blank_line_before_selected_text()
-> anyhow::Result<()> {
    let view_model = composer_selection_view_model("abc\nX", 4..5, 3);
    assert_eq!(view_model.cursor_position, (1, 1));
    let mut terminal = Terminal::new(TestBackend::new(9, 6))?;
    terminal.draw(|frame| render_input(frame, frame.area(), &view_model))?;
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(3, 3)].symbol(), "a");
    assert_eq!(buffer[(5, 3)].symbol(), "c");
    assert_eq!(buffer[(3, 4)].symbol(), "X");
    assert_eq!(buffer[(3, 4)].bg, Theme::default().palette.selection_bg);
    assert_eq!(
        composer_cursor_position(Rect::new(0, 0, 9, 6), &view_model),
        Some((4, 4))
    );
    Ok(())
}
