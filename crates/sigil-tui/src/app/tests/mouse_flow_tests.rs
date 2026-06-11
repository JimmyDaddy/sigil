use super::*;
use crate::{
    mouse::{AppMouseOutcome, HitTarget, MouseInput, MouseInputKind},
    ui::LayoutSnapshot,
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
