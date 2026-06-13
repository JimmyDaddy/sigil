use super::*;
use crate::{
    mouse::{AppMouseOutcome, HitTarget, MouseInput, MouseInputKind},
    ui::{LayoutMode, LayoutSnapshot},
};
use ratatui::layout::Rect;

fn mouse(kind: MouseInputKind, column: u16, row: u16) -> MouseInput {
    MouseInput {
        column,
        row,
        kind,
        modifiers: KeyModifiers::NONE,
    }
}

fn point_in(area: Rect) -> (u16, u16) {
    (area.x, area.y)
}

fn slash_candidate_point(layout: &LayoutSnapshot, index: usize) -> (u16, u16) {
    let slash = layout.slash_overlay.expect("expected slash overlay");
    assert!(index >= slash.window_start && index < slash.window_end);
    (
        slash.content.x,
        slash
            .content
            .y
            .saturating_add(slash.title_rows)
            .saturating_add(index.saturating_sub(slash.window_start) as u16),
    )
}

fn slash_candidate_point_by_label(
    app: &AppState,
    layout: &LayoutSnapshot,
    label: &str,
) -> (u16, u16) {
    let index = app
        .slash_selector_rows()
        .iter()
        .position(|(candidate, _)| candidate == label)
        .expect("expected slash candidate");
    slash_candidate_point(layout, index)
}

fn tool_card_point(layout: &LayoutSnapshot, entry_index: usize) -> (u16, u16) {
    let hit_area = layout
        .tool_cards
        .iter()
        .find(|area| area.entry_index == entry_index)
        .expect("expected visible tool card hit area");
    (hit_area.area.x, hit_area.area.y)
}

fn live_text_point_containing(
    app: &AppState,
    layout: &LayoutSnapshot,
    expected_text: &str,
) -> (u16, u16) {
    let hit_area = layout
        .live_text_rows
        .iter()
        .find(|hit| {
            app.timeline_plain_cache
                .get(hit.line_index)
                .is_some_and(|line| line.contains(expected_text))
        })
        .expect("expected visible live text row containing text");
    (hit_area.area.x, hit_area.area.y)
}

fn push_sample_tool_cards(app: &mut AppState) {
    app.push_timeline(
        TimelineRole::Tool,
        r#"{
  "call_id": "call-first",
  "tool_name": "ls",
  "status": "ok",
  "preview_kind": "json",
  "summary": "first 1/1 lines - 8 B",
  "preview_lines": ["[\".git\"]"],
  "preview_value": [".git"],
  "hidden_lines": 0
}"#,
    );
    app.push_timeline(
        TimelineRole::Tool,
        r#"{
  "call_id": "call-second",
  "tool_name": "ls",
  "status": "ok",
  "preview_kind": "json",
  "summary": "first 1/1 lines - 11 B",
  "preview_lines": ["[\"src/lib.rs\"]"],
  "preview_value": ["src/lib.rs"],
  "hidden_lines": 0
}"#,
    );
}

#[test]
fn layout_snapshot_hits_main_regions() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);

    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);

    let (live_x, live_y) = point_in(layout.live_panel);
    assert_eq!(layout.hit_target(live_x, live_y), HitTarget::LivePanel);

    let (composer_x, composer_y) = point_in(layout.composer);
    assert_eq!(
        layout.hit_target(composer_x, composer_y),
        HitTarget::Composer
    );

    let (rail_x, rail_y) = point_in(layout.info_rail);
    assert_eq!(layout.hit_target(rail_x, rail_y), HitTarget::InfoRail);
}

#[test]
fn layout_snapshot_hits_slash_candidate_over_live_panel() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.input = "/".to_owned();
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (column, row) = slash_candidate_point(&layout, 1);

    assert_eq!(
        layout.hit_target(column, row),
        HitTarget::SlashCandidate { index: 1 }
    );
}

#[test]
fn layout_snapshot_hits_visible_tool_cards_over_live_panel() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    push_sample_tool_cards(&mut app);
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let first_entry_index = app.tool_activity_entry_indices()[0];
    let (column, row) = tool_card_point(&layout, first_entry_index);

    assert_eq!(
        layout.hit_target(column, row),
        HitTarget::ToolCard {
            entry_index: first_entry_index
        }
    );
}

#[test]
fn mouse_drag_selects_live_text_and_ctrl_c_copies_selection() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.push_timeline(TimelineRole::User, "first selectable line");
    app.push_timeline(TimelineRole::Assistant, "second selectable line");
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    assert!(layout.live_text_rows.len() >= 2);
    let (start_column, start_row) =
        live_text_point_containing(&app, &layout, "first selectable line");
    let (end_column, end_row) = live_text_point_containing(&app, &layout, "second selectable line");

    let down = app.handle_mouse_event(
        mouse(MouseInputKind::LeftDown, start_column, start_row),
        &layout,
    )?;
    assert!(matches!(
        down,
        AppMouseOutcome::Noop | AppMouseOutcome::Redraw
    ));

    let drag = app.handle_mouse_event(mouse(MouseInputKind::Drag, end_column, end_row), &layout)?;
    assert!(matches!(drag, AppMouseOutcome::Redraw));

    let up = app.handle_mouse_event(mouse(MouseInputKind::LeftUp, end_column, end_row), &layout)?;
    assert!(matches!(up, AppMouseOutcome::Redraw));
    let selected_text = app
        .selected_timeline_text()
        .expect("expected selected timeline text");
    assert!(selected_text.contains("first selectable line"));
    assert!(!app.transcript_lines(usize::MAX).is_empty());

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))?;
    assert!(matches!(
        action,
        Some(AppAction::CopyToClipboard { text }) if text == selected_text
    ));
    assert_eq!(app.last_notice(), Some("copied selection"));
    Ok(())
}

#[test]
fn mouse_selection_edges_cover_reselect_clear_drag_and_idle_left_up() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.push_timeline(TimelineRole::User, "alpha selectable line");
    app.push_timeline(TimelineRole::Assistant, "beta selectable line");
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (alpha_column, alpha_row) =
        live_text_point_containing(&app, &layout, "alpha selectable line");
    let (beta_column, beta_row) = live_text_point_containing(&app, &layout, "beta selectable line");

    let idle_up = app.handle_mouse_event(
        mouse(MouseInputKind::LeftUp, alpha_column, alpha_row),
        &layout,
    )?;
    assert!(matches!(idle_up, AppMouseOutcome::Noop));

    let _ = app.handle_mouse_event(
        mouse(MouseInputKind::LeftDown, alpha_column, alpha_row),
        &layout,
    )?;
    let drag =
        app.handle_mouse_event(mouse(MouseInputKind::Drag, beta_column, beta_row), &layout)?;
    assert!(matches!(drag, AppMouseOutcome::Redraw));
    let repeated_drag =
        app.handle_mouse_event(mouse(MouseInputKind::Drag, beta_column, beta_row), &layout)?;
    assert!(matches!(repeated_drag, AppMouseOutcome::Noop));

    let reselect = app.handle_mouse_event(
        mouse(MouseInputKind::LeftDown, alpha_column, alpha_row),
        &layout,
    )?;
    assert!(matches!(reselect, AppMouseOutcome::Redraw));

    let (footer_column, footer_row) = point_in(layout.footer);
    let cleared = app.handle_mouse_event(
        mouse(MouseInputKind::LeftDown, footer_column, footer_row),
        &layout,
    )?;
    assert!(matches!(cleared, AppMouseOutcome::Redraw));
    assert!(app.selected_timeline_text().is_none());
    Ok(())
}

#[test]
fn timeline_text_selection_helpers_cover_invalid_and_empty_states() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);

    assert!(!app.update_timeline_text_selection(0));
    app.timeline_plain_cache.clear();
    app.timeline_text_selection_anchor = Some(0);
    assert!(!app.update_timeline_text_selection(0));

    app.push_timeline(TimelineRole::User, "line");
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let first_line = layout
        .live_text_rows
        .first()
        .expect("expected live text row")
        .line_index;
    assert!(!app.begin_timeline_text_selection(first_line));
    assert!(app.update_timeline_text_selection(first_line));
    assert!(app.begin_timeline_text_selection(usize::MAX));
    assert!(app.selected_timeline_text().is_none());
}

#[test]
fn mouse_click_composer_focuses_composer() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.active_pane = PaneFocus::Activity;
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (column, row) = point_in(layout.composer);

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert_eq!(app.active_pane, PaneFocus::Composer);
    Ok(())
}

#[test]
fn mouse_click_tool_card_selects_without_toggling() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.input = "draft".to_owned();
    push_sample_tool_cards(&mut app);
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let first_entry_index = app.tool_activity_entry_indices()[0];
    let (column, row) = tool_card_point(&layout, first_entry_index);

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert_eq!(
        app.selected_tool_activity_key,
        Some("call:call-first".to_owned())
    );
    assert_eq!(app.input, "draft");
    assert!(app.expanded_tool_activity_keys.is_empty());
    assert!(app.collapsed_tool_activity_keys.is_empty());
    Ok(())
}

#[test]
fn mouse_click_tool_card_without_text_hit_clears_selection() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.push_timeline(TimelineRole::User, "selected text");
    push_sample_tool_cards(&mut app);
    let mut layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (text_column, text_row) = live_text_point_containing(&app, &layout, "selected text");
    let _ = app.handle_mouse_event(
        mouse(MouseInputKind::LeftDown, text_column, text_row),
        &layout,
    )?;
    let _ = app.handle_mouse_event(mouse(MouseInputKind::Drag, text_column, text_row), &layout)?;
    assert!(app.selected_timeline_text().is_some());

    layout.live_text_rows.clear();
    let first_entry_index = app.tool_activity_entry_indices()[0];
    let (column, row) = tool_card_point(&layout, first_entry_index);
    let outcome = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert!(app.selected_timeline_text().is_none());
    Ok(())
}

#[test]
fn mouse_click_regular_slash_candidate_executes() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.input = "/".to_owned();
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (column, row) = slash_candidate_point_by_label(&app, &layout, "/config");

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert_eq!(app.active_pane, PaneFocus::Composer);
    assert!(app.is_config_mode());
    assert!(app.input.is_empty());
    Ok(())
}

#[test]
fn mouse_click_dangerous_slash_candidate_requires_second_click() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.input = "/".to_owned();
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (column, row) = slash_candidate_point_by_label(&app, &layout, "/compact");

    let first = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(first, AppMouseOutcome::Redraw));
    assert_eq!(app.input, "/compact");
    assert_eq!(app.last_notice(), Some("click again to confirm /compact"));

    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (column, row) = slash_candidate_point_by_label(&app, &layout, "/compact");
    let second = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(
        second,
        AppMouseOutcome::Action(AppAction::CompactNow)
    ));
    assert!(app.input.is_empty());
    Ok(())
}

#[test]
fn mouse_click_quit_shows_confirmation_in_slash_row() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.input = "/".to_owned();
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (column, row) = slash_candidate_point_by_label(&app, &layout, "/quit");

    let first = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(first, AppMouseOutcome::Redraw));
    assert!(!app.should_quit);
    assert_eq!(app.input, "/quit");
    let rows = app.slash_selector_rows();
    let (_, description) = rows
        .iter()
        .find(|(label, _)| label == "/quit")
        .expect("expected /quit row");
    assert!(description.starts_with("click again to confirm /quit"));
    Ok(())
}

#[test]
fn mouse_click_slash_candidate_is_noop_when_approval_is_pending() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.active_pane = PaneFocus::Activity;
    inject_write_file_approval(&mut app, sample_approval_preview())?;
    app.input = "/".to_owned();

    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (column, row) = slash_candidate_point_by_label(&app, &layout, "/config");

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Noop));
    assert_eq!(app.active_pane, PaneFocus::Activity);
    assert_eq!(app.input, "/");
    Ok(())
}

#[test]
fn mouse_click_tool_card_is_noop_when_approval_is_pending() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    push_sample_tool_cards(&mut app);
    inject_write_file_approval(&mut app, sample_approval_preview())?;
    let previous_selected_tool_activity_key = app.selected_tool_activity_key.clone();
    let first_entry_index = app.tool_activity_entry_indices()[0];
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (column, row) = tool_card_point(&layout, first_entry_index);

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Noop));
    assert_eq!(
        app.selected_tool_activity_key,
        previous_selected_tool_activity_key
    );
    Ok(())
}

#[test]
fn mouse_click_composer_is_noop_when_approval_is_pending() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.active_pane = PaneFocus::Activity;
    inject_write_file_approval(&mut app, sample_approval_preview())?;
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (column, row) = point_in(layout.composer);

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Noop));
    assert_eq!(app.active_pane, PaneFocus::Activity);
    Ok(())
}

#[test]
fn mouse_click_background_path_is_noop_without_state_change() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(80, 10);
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 80, 10), &app);
    let background = (layout.screen.width - 1, layout.screen.height - 1);

    let outcome = app.handle_mouse_event(
        mouse(MouseInputKind::LeftDown, background.0, background.1),
        &layout,
    )?;

    assert!(matches!(outcome, AppMouseOutcome::Noop));
    assert_eq!(
        layout.hit_target(background.0, background.1),
        HitTarget::Background
    );
    Ok(())
}

#[test]
fn keyboard_enter_dangerous_slash_command_needs_no_mouse_confirmation() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/compact".to_owned();

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(matches!(action, Some(AppAction::CompactNow)));
    assert_eq!(app.last_notice(), Some("compact requested"));
    Ok(())
}

#[test]
fn mouse_scroll_live_panel_moves_timeline_even_when_activity_focused() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(80, 12);
    app.active_pane = PaneFocus::Activity;
    for index in 0..8 {
        app.push_timeline(TimelineRole::Assistant, format!("message {index}"));
    }
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 80, 12), &app);
    let (column, row) = point_in(layout.live_panel);

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::ScrollUp, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert!(app.timeline_scroll_back > 0);
    assert_eq!(app.active_pane, PaneFocus::Activity);
    Ok(())
}

#[test]
fn mouse_scroll_slash_overlay_moves_candidate_selection() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.input = "/".to_owned();
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let slash = layout.slash_overlay.expect("expected slash overlay");
    let (column, row) = point_in(slash.overlay);

    let outcome =
        app.handle_mouse_event(mouse(MouseInputKind::ScrollDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert_eq!(app.active_pane, PaneFocus::Composer);
    assert_eq!(app.slash_selector_selected_index(), Some(1));
    Ok(())
}

#[test]
fn mouse_scroll_info_rail_focuses_activity_and_moves_sidebar() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (column, row) = point_in(layout.info_rail);

    let outcome =
        app.handle_mouse_event(mouse(MouseInputKind::ScrollDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert_eq!(app.active_pane, PaneFocus::Activity);
    assert_eq!(app.sidebar_selected_card.label(), "agents");
    Ok(())
}

#[test]
fn mouse_scroll_approval_modal_moves_approval_scroll() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    inject_write_file_approval(&mut app, sample_approval_preview())?;
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let approval = layout.approval_modal.expect("expected approval area");
    let (column, row) = point_in(approval);

    let outcome =
        app.handle_mouse_event(mouse(MouseInputKind::ScrollDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert!(app.approval_scroll_back > 0);
    Ok(())
}

#[test]
fn mouse_scroll_behind_approval_modal_is_noop() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    inject_write_file_approval(&mut app, sample_approval_preview())?;
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    assert_eq!(layout.hit_target(0, 0), HitTarget::LivePanel);

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::ScrollDown, 0, 0), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Noop));
    assert_eq!(app.approval_scroll_back, 0);
    assert_eq!(app.timeline_scroll_back, 0);
    Ok(())
}

#[test]
fn mouse_click_infotrack_focuses_activity() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.active_pane = PaneFocus::Composer;
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (column, row) = point_in(layout.info_rail);

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert_eq!(app.active_pane, PaneFocus::Activity);
    Ok(())
}

#[test]
fn mouse_click_unknown_tool_card_is_noop() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    push_sample_tool_cards(&mut app);
    let mut layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    layout.tool_cards[0].entry_index = 99;
    let (column, row) = point_in(layout.tool_cards[0].area);
    let previous_selected = app.selected_tool_activity_key.clone();

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Noop));
    assert_eq!(app.selected_tool_activity_key, previous_selected);
    Ok(())
}

#[test]
fn mouse_scroll_approval_modal_hit_when_no_pending_is_noop() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(80, 20);
    let layout = LayoutSnapshot {
        screen: Rect::new(0, 0, 80, 20),
        mode: LayoutMode::Main,
        live_panel: Rect::new(0, 0, 80, 12),
        composer: Rect::new(0, 12, 80, 4),
        footer: Rect::new(0, 16, 80, 4),
        info_rail: Rect::new(60, 0, 20, 12),
        live_text_rows: Vec::new(),
        tool_cards: Vec::new(),
        slash_overlay: None,
        approval_modal: Some(Rect::new(10, 2, 20, 6)),
        approval_modal_hit_areas: None,
    };
    let (column, row) = point_in(layout.approval_modal.expect("expected approval area"));

    let outcome =
        app.handle_mouse_event(mouse(MouseInputKind::ScrollDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Noop));
    Ok(())
}

#[test]
fn mouse_scroll_composer_hit_when_no_pending_is_noop() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(80, 20);
    let layout = LayoutSnapshot {
        screen: Rect::new(0, 0, 80, 20),
        mode: LayoutMode::Main,
        live_panel: Rect::new(0, 0, 80, 12),
        composer: Rect::new(0, 12, 80, 4),
        footer: Rect::new(0, 16, 80, 4),
        info_rail: Rect::new(60, 0, 20, 12),
        live_text_rows: Vec::new(),
        tool_cards: Vec::new(),
        slash_overlay: None,
        approval_modal: None,
        approval_modal_hit_areas: None,
    };
    let (column, row) = point_in(layout.composer);

    let outcome =
        app.handle_mouse_event(mouse(MouseInputKind::ScrollDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Noop));
    Ok(())
}

#[test]
fn mouse_drag_is_noop_by_default() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.set_terminal_size(120, 20);
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (column, row) = point_in(layout.composer);

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::Drag, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Noop));
    Ok(())
}

#[test]
fn mouse_scroll_tool_card_scrolls_timeline() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(80, 12);
    for index in 0..8 {
        app.push_timeline(TimelineRole::Assistant, format!("message {index}"));
    }
    push_sample_tool_cards(&mut app);
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 80, 12), &app);
    let first_entry_index = app.tool_activity_entry_indices()[0];
    let (column, row) = tool_card_point(&layout, first_entry_index);
    let before = app.timeline_scroll_back;

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::ScrollUp, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert!(app.timeline_scroll_back > before);
    Ok(())
}

#[test]
fn mouse_scroll_approval_with_pending_approval_scrolls_upward() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    inject_write_file_approval(&mut app, sample_approval_preview())?;
    app.approval_scroll_back = 5;
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let approval = layout.approval_modal.expect("expected approval area");
    let (column, row) = point_in(approval);

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::ScrollUp, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert_eq!(app.approval_scroll_back, 2);
    Ok(())
}

#[test]
fn mouse_click_approval_file_row_selects_file() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 24);
    inject_write_file_approval(&mut app, multi_file_approval_preview())?;
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 24), &app);
    let hit_areas = layout
        .approval_modal_hit_areas
        .as_ref()
        .expect("expected approval hit areas");
    let second_file = hit_areas.file_rows[1];
    let (column, row) = point_in(second_file.area);

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert_eq!(app.approval_selected_file_index, 1);
    assert_eq!(app.approval_selected_hunk_index, 0);
    assert_eq!(app.approval_scroll_back, 0);
    Ok(())
}

#[test]
fn mouse_click_selected_or_missing_approval_file_is_noop() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 24);
    inject_write_file_approval(&mut app, multi_file_approval_preview())?;
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 24), &app);
    let first_file = layout
        .approval_modal_hit_areas
        .as_ref()
        .expect("expected approval hit areas")
        .file_rows[0];
    let (column, row) = point_in(first_file.area);

    let same = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(same, AppMouseOutcome::Noop));

    app.pending_approval
        .as_mut()
        .and_then(|pending| pending.preview.as_mut())
        .expect("expected preview")
        .file_diffs
        .clear();
    let missing_file =
        app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(missing_file, AppMouseOutcome::Noop));
    Ok(())
}

#[test]
fn mouse_scroll_approval_file_row_scrolls_modal() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 24);
    inject_write_file_approval(&mut app, multi_file_approval_preview())?;
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 24), &app);
    let first_file = layout
        .approval_modal_hit_areas
        .as_ref()
        .expect("expected approval hit areas")
        .file_rows[0];
    let (column, row) = point_in(first_file.area);

    let outcome =
        app.handle_mouse_event(mouse(MouseInputKind::ScrollDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert_eq!(app.approval_scroll_back, 3);
    Ok(())
}

#[test]
fn mouse_click_approval_diff_controls_update_modal_state() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 24);
    inject_write_file_approval(&mut app, multi_file_approval_preview())?;
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 24), &app);
    let hit_areas = layout
        .approval_modal_hit_areas
        .as_ref()
        .expect("expected approval hit areas");

    let (column, row) = point_in(hit_areas.hunk_next);
    let hunk_next =
        app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;
    assert!(matches!(hunk_next, AppMouseOutcome::Redraw));
    assert_eq!(app.approval_selected_hunk_index, 1);
    assert!(app.approval_scroll_back > 0);

    let (column, row) = point_in(hit_areas.hunk_previous);
    let hunk_previous =
        app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;
    assert!(matches!(hunk_previous, AppMouseOutcome::Redraw));
    assert_eq!(app.approval_selected_hunk_index, 0);

    let (column, row) = point_in(hit_areas.diff_view_toggle);
    let view_toggle =
        app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;
    assert!(matches!(view_toggle, AppMouseOutcome::Redraw));
    assert_eq!(app.approval_diff_mode.label(), "current-hunk");
    assert_eq!(app.approval_scroll_back, 0);

    let (column, row) = point_in(hit_areas.metadata_toggle);
    let metadata_toggle =
        app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;
    assert!(matches!(metadata_toggle, AppMouseOutcome::Redraw));
    assert!(app.approval_metadata_collapsed);
    Ok(())
}

#[test]
fn mouse_click_approval_hunk_control_without_available_move_is_noop() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 24);
    inject_write_file_approval(&mut app, sample_approval_preview())?;
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 24), &app);
    let hit_areas = layout
        .approval_modal_hit_areas
        .as_ref()
        .expect("expected approval hit areas");
    let (column, row) = point_in(hit_areas.hunk_previous);

    let previous = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(previous, AppMouseOutcome::Noop));
    assert_eq!(app.approval_selected_hunk_index, 0);
    assert_eq!(app.approval_scroll_back, 0);

    let (column, row) = point_in(hit_areas.hunk_next);
    let next = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(next, AppMouseOutcome::Noop));
    assert_eq!(app.approval_selected_hunk_index, 0);
    assert_eq!(app.approval_scroll_back, 0);
    Ok(())
}

#[test]
fn approval_mouse_helpers_without_pending_approval_are_noop() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    assert!(!app.toggle_approval_metadata());
    assert!(!app.cycle_approval_diff_mode());
}

#[test]
fn mouse_scroll_stale_approval_action_target_without_pending_is_noop() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 24);
    inject_write_file_approval(&mut app, sample_approval_preview())?;
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 24), &app);
    let allow_action = layout
        .approval_modal_hit_areas
        .as_ref()
        .expect("expected approval hit areas")
        .allow_action;
    app.pending_approval = None;
    let (column, row) = point_in(allow_action);

    let outcome =
        app.handle_mouse_event(mouse(MouseInputKind::ScrollDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Noop));
    Ok(())
}

#[test]
fn mouse_click_approval_action_returns_decision() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 24);
    inject_write_file_approval(&mut app, sample_approval_preview())?;
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 24), &app);
    let allow_action = layout
        .approval_modal_hit_areas
        .as_ref()
        .expect("expected approval hit areas")
        .allow_action;
    let (column, row) = point_in(allow_action);

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(
        outcome,
        AppMouseOutcome::Action(AppAction::ApprovalDecision {
            call_id,
            approved: true
        }) if call_id == "call-1"
    ));
    Ok(())
}
