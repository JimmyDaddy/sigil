use super::*;
use crate::{
    mouse::{AppMouseOutcome, HitTarget, MouseInput, MouseInputKind},
    ui::{LayoutMode, LayoutSnapshot},
};
use ratatui::{layout::Rect, text::Line};
use unicode_width::UnicodeWidthStr;

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

fn rendered_plain(lines: Vec<Line<'static>>) -> String {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
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

fn tool_card_header_point(layout: &LayoutSnapshot, entry_index: usize) -> (u16, u16) {
    let hit_area = layout
        .tool_cards
        .iter()
        .find(|area| area.entry_index == entry_index)
        .expect("expected visible tool card hit area");
    point_in(
        hit_area
            .header_area
            .expect("expected visible tool card header"),
    )
}

fn tool_card_body_point(layout: &LayoutSnapshot, entry_index: usize) -> (u16, u16) {
    let hit_area = layout
        .tool_cards
        .iter()
        .find(|area| area.entry_index == entry_index)
        .expect("expected visible tool card hit area");
    (
        hit_area.area.x,
        hit_area
            .area
            .y
            .saturating_add(u16::from(hit_area.area.height > 1)),
    )
}

fn tool_card_hidden_preview_point(layout: &LayoutSnapshot, entry_index: usize) -> (u16, u16) {
    let hit_area = layout
        .tool_cards
        .iter()
        .find(|area| area.entry_index == entry_index)
        .expect("expected visible tool card hit area");
    point_in(
        hit_area
            .hidden_preview_area
            .expect("expected visible hidden preview hit area"),
    )
}

fn thinking_header_point(layout: &LayoutSnapshot, entry_index: usize) -> (u16, u16) {
    let hit_area = layout
        .thinking_blocks
        .iter()
        .find(|area| area.entry_index == entry_index)
        .expect("expected visible thinking block hit area");
    point_in(
        hit_area
            .header_area
            .expect("expected visible thinking block header"),
    )
}

fn thinking_block_body_point(layout: &LayoutSnapshot, entry_index: usize) -> (u16, u16) {
    let hit_area = layout
        .thinking_blocks
        .iter()
        .find(|area| area.entry_index == entry_index)
        .expect("expected visible thinking block hit area");
    (
        hit_area.area.x,
        hit_area
            .area
            .y
            .saturating_add(u16::from(hit_area.area.height > 1)),
    )
}

fn setup_field_point(layout: &LayoutSnapshot, index: usize) -> (u16, u16) {
    let hit_area = layout
        .setup_hit_areas
        .as_ref()
        .expect("expected setup hit areas")
        .fields
        .iter()
        .find(|area| area.index == index)
        .expect("expected setup field hit area");
    point_in(hit_area.area)
}

fn config_section_point(layout: &LayoutSnapshot, index: usize) -> (u16, u16) {
    let hit_area = layout
        .config_hit_areas
        .as_ref()
        .expect("expected config hit areas")
        .sections
        .iter()
        .find(|area| area.index == index)
        .expect("expected config section hit area");
    point_in(hit_area.area)
}

fn config_field_point(layout: &LayoutSnapshot, index: usize) -> (u16, u16) {
    let hit_area = layout
        .config_hit_areas
        .as_ref()
        .expect("expected config hit areas")
        .fields
        .iter()
        .find(|area| area.index == index)
        .expect("expected config field hit area");
    point_in(hit_area.area)
}

fn config_footer_action_point(layout: &LayoutSnapshot, index: usize) -> (u16, u16) {
    let hit_area = layout
        .config_hit_areas
        .as_ref()
        .expect("expected config hit areas")
        .footer_actions
        .iter()
        .find(|area| area.index == index)
        .expect("expected config footer action hit area");
    point_in(hit_area.area)
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
            app.timeline_plain_line(hit.line_index)
                .is_some_and(|line| line.contains(expected_text))
        })
        .expect("expected visible live text row containing text");
    (hit_area.area.x, hit_area.area.y)
}

fn live_text_point_at_text_offset(
    app: &AppState,
    layout: &LayoutSnapshot,
    expected_text: &str,
    offset: usize,
) -> (u16, u16) {
    let hit_area = layout
        .live_text_rows
        .iter()
        .find(|hit| {
            app.timeline_plain_line(hit.line_index)
                .is_some_and(|line| line.contains(expected_text))
        })
        .expect("expected visible live text row containing text");
    let line = app
        .timeline_plain_line(hit_area.line_index)
        .expect("expected plain timeline line");
    let text_start = line.find(expected_text).expect("expected text in line");
    let text_start_width = UnicodeWidthStr::width(&line[..text_start]);
    (
        hit_area
            .area
            .x
            .saturating_add(text_start_width.saturating_add(offset) as u16),
        hit_area.area.y,
    )
}

fn push_sample_tool_cards(app: &mut AppState) {
    app.push_timeline(
        TimelineRole::Tool,
        r#"{
  "call_id": "call-first",
  "tool_name": "ls",
  "status": "ok",
  "preview_kind": "json",
  "summary": "first 1/3 lines - 8 B",
  "preview_lines": ["[\".git\"]"],
  "preview_value": [".git"],
  "hidden_lines": 2
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

fn push_long_tool_card(app: &mut AppState, call_id: &str, line_count: usize) {
    let preview_lines = (1..=line_count)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>();
    app.push_timeline(
        TimelineRole::Tool,
        json!({
            "call_id": call_id,
            "tool_name": "read_file",
            "status": "ok",
            "preview_kind": "text",
            "summary": format!("first {line_count}/{line_count} lines - 240 B"),
            "preview_lines": preview_lines,
            "hidden_lines": 0
        })
        .to_string(),
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
fn checkpoint_restore_modal_blocks_mouse_passthrough_to_composer() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.composer.input = "preserved draft".to_owned();
    app.modal_state = Some(ModalState::CheckpointRestore(
        super::super::checkpoint_flow::CheckpointRestoreModalState::default(),
    ));
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (column, row) = point_in(layout.composer_input);

    let click = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;
    assert!(matches!(click, AppMouseOutcome::Noop));
    assert_eq!(app.composer.input, "preserved draft");
    assert!(app.checkpoint_restore_modal_open());

    let scroll = app.handle_mouse_event(mouse(MouseInputKind::ScrollDown, column, row), &layout)?;
    assert!(matches!(scroll, AppMouseOutcome::Redraw));
    assert_eq!(app.composer.input, "preserved draft");
    Ok(())
}

#[test]
fn mouse_click_setup_field_selects_then_activates() -> Result<()> {
    let temp = tempdir()?;
    let mut app = AppState::from_setup(
        temp.path().join("sigil.toml"),
        temp.path().to_path_buf(),
        None,
    );
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let model_index = 1;
    let (column, row) = setup_field_point(&layout, model_index);

    let first = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(first, AppMouseOutcome::Redraw));
    assert_eq!(
        app.setup_state
            .as_ref()
            .expect("expected setup state")
            .selected_field,
        SetupField::Model
    );
    assert!(!app.has_modal());

    let second = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(second, AppMouseOutcome::Redraw));
    assert!(app.has_modal());
    Ok(())
}

#[test]
fn mouse_click_setup_save_runs_validation() -> Result<()> {
    let temp = tempdir()?;
    let mut app = AppState::from_setup(
        temp.path().join("sigil.toml"),
        temp.path().to_path_buf(),
        None,
    );
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let save_index = 3;
    let (column, row) = setup_field_point(&layout, save_index);

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert_eq!(
        app.last_notice(),
        Some("trust the current folder before starting sigil")
    );
    assert_eq!(
        app.setup_state
            .as_ref()
            .expect("expected setup state")
            .selected_field,
        SetupField::Save
    );
    Ok(())
}

#[test]
fn mouse_click_config_section_selects_step() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.composer.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let memory_index = ConfigSection::Memory
        .flow_index()
        .expect("memory section should have index");
    let (column, row) = config_section_point(&layout, memory_index);

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert_eq!(
        app.config_state
            .as_ref()
            .expect("expected config state")
            .selected_section,
        ConfigSection::Memory
    );
    Ok(())
}

#[test]
fn mouse_click_config_field_selects_then_activates() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.composer.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let provider_index = ConfigField::fields_for_section(ConfigSection::Provider)
        .iter()
        .position(|field| *field == ConfigField::ProviderName)
        .expect("expected provider field index");
    let (column, row) = config_field_point(&layout, provider_index);

    let first = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(first, AppMouseOutcome::Redraw));
    let state = app.config_state.as_ref().expect("expected config state");
    assert_eq!(state.selected_field, Some(ConfigField::ProviderName));
    assert!(!state.dirty);

    let second = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(second, AppMouseOutcome::Redraw));
    let state = app.config_state.as_ref().expect("expected config state");
    assert_eq!(state.draft.provider_name, "openai_compat");
    assert!(state.dirty);
    Ok(())
}

#[test]
fn mouse_click_config_footer_action_executes() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.composer.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let close_index = ConfigFooterAction::actions_for_section(ConfigSection::Provider)
        .iter()
        .position(|action| *action == ConfigFooterAction::Close)
        .expect("expected close footer action");
    let (column, row) = config_footer_action_point(&layout, close_index);

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert!(!app.is_config_mode());
    assert_eq!(app.last_notice(), Some("closed config"));
    Ok(())
}

#[test]
fn mouse_click_setup_and_config_invalid_targets_are_noops() -> Result<()> {
    let temp = tempdir()?;
    let mut setup_app = AppState::from_setup(
        temp.path().join("sigil.toml"),
        temp.path().to_path_buf(),
        None,
    );
    let mut setup_layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &setup_app);
    let setup_field = setup_layout
        .setup_hit_areas
        .as_mut()
        .expect("expected setup hit areas")
        .fields
        .first_mut()
        .expect("expected setup field area");
    setup_field.index = 99;
    let (column, row) = point_in(setup_field.area);

    let setup_outcome = setup_app
        .handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &setup_layout)?;

    assert!(matches!(setup_outcome, AppMouseOutcome::Noop));

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.composer.input = "/config".to_owned();
    let _ = app.submit_input()?;

    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (column, row) = config_section_point(&layout, 0);
    let same_section =
        app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;
    assert!(matches!(same_section, AppMouseOutcome::Noop));

    let mut invalid_section_layout = layout.clone();
    invalid_section_layout
        .config_hit_areas
        .as_mut()
        .expect("expected config hit areas")
        .sections[0]
        .index = 99;
    let (column, row) = point_in(
        invalid_section_layout
            .config_hit_areas
            .as_ref()
            .expect("expected config hit areas")
            .sections[0]
            .area,
    );
    let invalid_section = app.handle_mouse_event(
        mouse(MouseInputKind::LeftDown, column, row),
        &invalid_section_layout,
    )?;
    assert!(matches!(invalid_section, AppMouseOutcome::Noop));

    let mut invalid_field_layout = layout.clone();
    invalid_field_layout
        .config_hit_areas
        .as_mut()
        .expect("expected config hit areas")
        .fields[0]
        .index = 99;
    let (column, row) = point_in(
        invalid_field_layout
            .config_hit_areas
            .as_ref()
            .expect("expected config hit areas")
            .fields[0]
            .area,
    );
    let invalid_field = app.handle_mouse_event(
        mouse(MouseInputKind::LeftDown, column, row),
        &invalid_field_layout,
    )?;
    assert!(matches!(invalid_field, AppMouseOutcome::Noop));

    let mut invalid_footer_layout = layout.clone();
    invalid_footer_layout
        .config_hit_areas
        .as_mut()
        .expect("expected config hit areas")
        .footer_actions[0]
        .index = 99;
    let (column, row) = point_in(
        invalid_footer_layout
            .config_hit_areas
            .as_ref()
            .expect("expected config hit areas")
            .footer_actions[0]
            .area,
    );
    let invalid_footer = app.handle_mouse_event(
        mouse(MouseInputKind::LeftDown, column, row),
        &invalid_footer_layout,
    )?;
    assert!(matches!(invalid_footer, AppMouseOutcome::Noop));
    Ok(())
}

#[test]
fn mouse_click_config_field_is_noop_when_mcp_has_no_servers() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.composer.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (column, row) = config_field_point(&layout, 0);

    app.config_state
        .as_mut()
        .expect("expected config state")
        .set_section(ConfigSection::Mcp);
    let outcome = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Noop));
    assert_eq!(
        app.config_state
            .as_ref()
            .expect("expected config state")
            .selected_field,
        None
    );
    Ok(())
}

#[test]
fn mouse_click_resume_session_selector_switches_session() -> Result<()> {
    let temp = tempdir()?;
    let config = RootConfig {
        workspace: WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        ..test_config()
    };
    let session_dir = resolved_session_log_dir(&config, temp.path());
    std::fs::create_dir_all(&session_dir)?;
    let restored_path = session_dir.join("session-restored.jsonl");
    let restored = restored_entries("restored-provider", "restored-model");
    write_session_log(&restored_path, &restored)?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.set_terminal_size(120, 20);
    app.composer.input = "/resume".to_owned();
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (column, row) = slash_candidate_point(&layout, 0);

    let first = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(first, AppMouseOutcome::Redraw));
    assert!(app.composer.input.starts_with("/resume "));
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (column, row) = slash_candidate_point(&layout, 0);
    let outcome = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;
    assert!(matches!(
        outcome,
        AppMouseOutcome::Action(AppAction::SwitchSession { session_log_path })
            if session_log_path == restored_path
    ));
    Ok(())
}

#[test]
fn layout_snapshot_hits_slash_candidate_over_live_panel() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.composer.input = "/".to_owned();
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
    let (column, row) = tool_card_header_point(&layout, first_entry_index);

    assert_eq!(
        layout.hit_target(column, row),
        HitTarget::ToolCardHeader {
            entry_index: first_entry_index
        }
    );

    let (column, row) = tool_card_body_point(&layout, first_entry_index);

    assert_eq!(
        layout.hit_target(column, row),
        HitTarget::ToolCard {
            entry_index: first_entry_index
        }
    );

    let (column, row) = tool_card_hidden_preview_point(&layout, first_entry_index);

    assert_eq!(
        layout.hit_target(column, row),
        HitTarget::ToolCardHiddenPreview {
            entry_index: first_entry_index
        }
    );
}

#[test]
fn layout_snapshot_hits_visible_thinking_block_over_live_panel() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.handle(RunEvent::ReasoningDelta(
        "planning step 1\nplanning step 2\nplanning step 3\nplanning step 4\nplanning step 5"
            .to_owned(),
    ))?;
    app.handle(RunEvent::ToolCallStarted(ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    }))?;

    let thinking_entry_index = app.collapsible_thinking_entry_indices()[0];
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (column, row) = thinking_header_point(&layout, thinking_entry_index);

    assert_eq!(
        layout.hit_target(column, row),
        HitTarget::ThinkingBlock {
            entry_index: thinking_entry_index
        }
    );

    let (column, row) = thinking_block_body_point(&layout, thinking_entry_index);

    assert_eq!(
        layout.hit_target(column, row),
        HitTarget::ThinkingBlock {
            entry_index: thinking_entry_index
        }
    );
    Ok(())
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
    let (end_column, end_row) =
        live_text_point_at_text_offset(&app, &layout, "second selectable line", 80);

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
    assert!(
        app.last_notice()
            .is_some_and(|notice| notice.starts_with("copy pending "))
    );
    Ok(())
}

#[test]
fn mouse_drag_selects_timeline_text_by_columns() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.push_timeline(TimelineRole::User, "abcdef");
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (start_column, start_row) = live_text_point_at_text_offset(&app, &layout, "abcdef", 1);
    let (end_column, end_row) = live_text_point_at_text_offset(&app, &layout, "abcdef", 4);

    let _ = app.handle_mouse_event(
        mouse(MouseInputKind::LeftDown, start_column, start_row),
        &layout,
    )?;
    let drag = app.handle_mouse_event(mouse(MouseInputKind::Drag, end_column, end_row), &layout)?;
    assert!(matches!(drag, AppMouseOutcome::Redraw));

    assert_eq!(app.selected_timeline_text().as_deref(), Some("bcd"));
    let rendered = app.transcript_lines(usize::MAX);
    assert!(
        rendered
            .iter()
            .flat_map(|line| line.spans.iter())
            .any(|span| span.content.contains("bcd") && span.style.bg.is_some())
    );
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
    assert!(!app.update_timeline_text_selection_at(0, 0));
    app.timeline.clear();
    app.rebuild_timeline_render_store();
    app.timeline_text_selection_anchor = Some(0);
    assert!(!app.update_timeline_text_selection(0));
    assert!(!app.update_timeline_text_selection_at(0, 0));
    app.timeline_text_selection_anchor_column = Some(0);
    assert!(!app.update_timeline_text_selection_at(0, 0));

    app.push_timeline(TimelineRole::User, "line");
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let first_line = layout
        .live_text_rows
        .first()
        .expect("expected live text row")
        .line_index;
    assert!(!app.begin_timeline_text_selection_at(first_line, 0));
    assert!(app.update_timeline_text_selection(first_line));
    assert!(app.begin_timeline_text_selection_at(first_line, 1));
    assert!(app.update_timeline_text_selection_at(first_line, 1));
    assert!(app.selected_timeline_text().is_none());
    assert!(app.begin_timeline_text_selection_at(usize::MAX, 0));
    assert!(app.selected_timeline_text().is_none());
}

#[test]
fn mouse_click_composer_focuses_and_positions_cursor() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.composer.input = "hello world".to_owned();
    app.composer.input_cursor = app.input_char_len();
    app.active_pane = PaneFocus::Activity;
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let column = layout.composer_input.x.saturating_add(2);
    let row = layout.composer_input.y;

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert_eq!(app.active_pane, PaneFocus::Composer);
    assert_eq!(app.composer.input_cursor, 2);

    let unchanged =
        app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(unchanged, AppMouseOutcome::Redraw));
    assert_eq!(app.composer.input_cursor, 2);

    let mut composer_only_layout = layout.clone();
    composer_only_layout.composer_input = Rect::default();
    app.composer.input_cursor = 7;
    let focus_only = app.handle_mouse_event(
        mouse(
            MouseInputKind::LeftDown,
            composer_only_layout.composer.x,
            composer_only_layout.composer.y,
        ),
        &composer_only_layout,
    )?;

    assert!(matches!(focus_only, AppMouseOutcome::Redraw));
    assert_eq!(app.composer.input_cursor, 7);
    Ok(())
}

#[test]
fn mouse_press_tool_card_body_selects_without_toggling() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.composer.input = "draft".to_owned();
    push_sample_tool_cards(&mut app);
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let first_entry_index = app.tool_activity_entry_indices()[0];
    let (column, row) = tool_card_body_point(&layout, first_entry_index);

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert_eq!(
        app.selected_tool_activity_key,
        Some("call:call-first".to_owned())
    );
    assert_eq!(app.active_pane, PaneFocus::Activity);
    assert_eq!(app.composer.input, "draft");
    assert!(app.expanded_tool_activity_keys.is_empty());
    assert!(app.collapsed_tool_activity_keys.is_empty());
    Ok(())
}

#[test]
fn mouse_click_tool_card_body_toggles_card_on_release() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    push_sample_tool_cards(&mut app);
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let first_entry_index = app.tool_activity_entry_indices()[0];
    let (column, row) = tool_card_body_point(&layout, first_entry_index);

    let down = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;
    assert!(matches!(down, AppMouseOutcome::Redraw));
    assert!(app.expanded_tool_activity_keys.is_empty());

    let up = app.handle_mouse_event(mouse(MouseInputKind::LeftUp, column, row), &layout)?;

    assert!(matches!(up, AppMouseOutcome::Redraw));
    assert_eq!(
        app.selected_tool_activity_key,
        Some("call:call-first".to_owned())
    );
    assert_eq!(app.active_pane, PaneFocus::Activity);
    assert!(app.expanded_tool_activity_keys.contains("call:call-first"));
    Ok(())
}

#[test]
fn mouse_drag_tool_card_body_does_not_toggle_card() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    push_sample_tool_cards(&mut app);
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let first_entry_index = app.tool_activity_entry_indices()[0];
    let (start_column, start_row) = tool_card_body_point(&layout, first_entry_index);
    let end_column = start_column.saturating_add(12);

    let _ = app.handle_mouse_event(
        mouse(MouseInputKind::LeftDown, start_column, start_row),
        &layout,
    )?;
    let drag =
        app.handle_mouse_event(mouse(MouseInputKind::Drag, end_column, start_row), &layout)?;
    let up = app.handle_mouse_event(
        mouse(MouseInputKind::LeftUp, end_column, start_row),
        &layout,
    )?;

    assert!(matches!(
        drag,
        AppMouseOutcome::Noop | AppMouseOutcome::Redraw
    ));
    assert!(matches!(
        up,
        AppMouseOutcome::Noop | AppMouseOutcome::Redraw
    ));
    assert!(app.expanded_tool_activity_keys.is_empty());
    Ok(())
}

#[test]
fn mouse_click_tool_card_header_toggles_card() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    push_sample_tool_cards(&mut app);
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let first_entry_index = app.tool_activity_entry_indices()[0];
    let (column, row) = tool_card_header_point(&layout, first_entry_index);

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert_eq!(
        app.selected_tool_activity_key,
        Some("call:call-first".to_owned())
    );
    assert_eq!(app.active_pane, PaneFocus::Activity);
    assert!(app.expanded_tool_activity_keys.contains("call:call-first"));
    assert_eq!(
        app.mouse_hover_target,
        Some(HitTarget::ToolCardHeader {
            entry_index: first_entry_index
        })
    );
    Ok(())
}

#[test]
fn mouse_release_only_tool_card_header_toggles_card() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    push_sample_tool_cards(&mut app);
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let first_entry_index = app.tool_activity_entry_indices()[0];
    let (column, row) = tool_card_header_point(&layout, first_entry_index);

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::LeftUp, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert_eq!(
        app.selected_tool_activity_key,
        Some("call:call-first".to_owned())
    );
    assert_eq!(app.active_pane, PaneFocus::Activity);
    assert!(app.expanded_tool_activity_keys.contains("call:call-first"));
    Ok(())
}

#[test]
fn mouse_click_tool_card_header_keeps_expanded_card_visible() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(100, 20);
    push_long_tool_card(&mut app, "call-long", 32);
    push_sample_tool_cards(&mut app);
    let long_entry_index = app.tool_activity_entry_indices()[0];
    app.reveal_timeline_entry(long_entry_index);
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 100, 20), &app);
    let (column, row) = tool_card_header_point(&layout, long_entry_index);

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert!(app.expanded_tool_activity_keys.contains("call:call-long"));
    let expanded_layout = LayoutSnapshot::from_app(Rect::new(0, 0, 100, 20), &app);
    let expanded_hit_area = expanded_layout
        .tool_cards
        .iter()
        .find(|area| area.entry_index == long_entry_index)
        .expect("expanded tool card should remain visible");
    assert_eq!(expanded_hit_area.header_area.map(|area| area.y), Some(row));
    Ok(())
}

#[test]
fn mouse_click_tool_card_hidden_preview_toggles_card() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    push_sample_tool_cards(&mut app);
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let first_entry_index = app.tool_activity_entry_indices()[0];
    let (column, row) = tool_card_hidden_preview_point(&layout, first_entry_index);

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert_eq!(
        app.selected_tool_activity_key,
        Some("call:call-first".to_owned())
    );
    assert_eq!(app.active_pane, PaneFocus::Activity);
    assert!(app.expanded_tool_activity_keys.contains("call:call-first"));
    assert_eq!(
        app.mouse_hover_target,
        Some(HitTarget::ToolCardHiddenPreview {
            entry_index: first_entry_index
        })
    );
    Ok(())
}

#[test]
fn mouse_move_tool_card_updates_hover_visual_state() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    push_sample_tool_cards(&mut app);
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let first_entry_index = app.tool_activity_entry_indices()[0];
    let (column, row) = tool_card_header_point(&layout, first_entry_index);
    let revision = app.timeline_revision();

    let hover = app.handle_mouse_event(mouse(MouseInputKind::Moved, column, row), &layout)?;

    assert!(matches!(hover, AppMouseOutcome::Redraw));
    assert_eq!(
        app.mouse_hover_target,
        Some(HitTarget::ToolCardHeader {
            entry_index: first_entry_index
        })
    );
    assert!(app.timeline_revision() > revision);

    let clear = app.handle_mouse_event(
        mouse(MouseInputKind::Moved, layout.footer.x, layout.footer.y),
        &layout,
    )?;

    assert!(matches!(clear, AppMouseOutcome::Redraw));
    assert_eq!(app.mouse_hover_target, None);
    assert_eq!(app.hovered_tool_activity_key(), None);
    app.mouse_hover_target = Some(HitTarget::Composer);
    assert_eq!(app.hovered_tool_activity_key(), None);
    app.mouse_hover_target = None;

    let no_change = app.handle_mouse_event(
        mouse(MouseInputKind::Moved, layout.footer.x, layout.footer.y),
        &layout,
    )?;

    assert!(matches!(no_change, AppMouseOutcome::Noop));
    assert_eq!(app.mouse_hover_target, None);
    Ok(())
}

#[test]
fn mouse_click_thinking_block_toggles_expansion() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 24);
    app.handle(RunEvent::ReasoningDelta(
        "planning step 1\nplanning step 2\nplanning step 3\nplanning step 4\nplanning step 5"
            .to_owned(),
    ))?;
    app.handle(RunEvent::ToolCallStarted(ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    }))?;
    let thinking_entry_index = app.collapsible_thinking_entry_indices()[0];
    let collapsed_plain = rendered_plain(app.transcript_lines(20));
    assert!(collapsed_plain.contains("thought"));
    assert!(collapsed_plain.contains("Ctrl-T expand"));
    assert!(!collapsed_plain.contains("planning step 5"));

    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 24), &app);
    let (column, row) = thinking_block_body_point(&layout, thinking_entry_index);
    let revision = app.timeline_revision();
    let hover = app.handle_mouse_event(mouse(MouseInputKind::Moved, column, row), &layout)?;

    assert!(matches!(hover, AppMouseOutcome::Redraw));
    assert_eq!(
        app.mouse_hover_target,
        Some(HitTarget::ThinkingBlock {
            entry_index: thinking_entry_index
        })
    );
    assert!(app.timeline_revision() > revision);

    let leave_revision = app.timeline_revision();
    let (composer_column, composer_row) = point_in(layout.composer);
    let leave = app.handle_mouse_event(
        mouse(MouseInputKind::Moved, composer_column, composer_row),
        &layout,
    )?;

    assert!(matches!(leave, AppMouseOutcome::Redraw));
    assert_eq!(app.mouse_hover_target, Some(HitTarget::Composer));
    assert!(app.timeline_revision() > leave_revision);

    let expand = app.handle_mouse_event(mouse(MouseInputKind::LeftUp, column, row), &layout)?;

    assert!(matches!(expand, AppMouseOutcome::Redraw));
    assert_eq!(
        app.mouse_hover_target,
        Some(HitTarget::ThinkingBlock {
            entry_index: thinking_entry_index
        })
    );
    let expanded_plain = rendered_plain(app.transcript_lines(20));
    assert!(expanded_plain.contains("Ctrl-T collapse"));
    assert!(expanded_plain.contains("planning step 5"));

    let expanded_layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 24), &app);
    let (column, row) = thinking_header_point(&expanded_layout, thinking_entry_index);
    let collapse = app.handle_mouse_event(
        mouse(MouseInputKind::LeftDown, column, row),
        &expanded_layout,
    )?;

    assert!(matches!(collapse, AppMouseOutcome::Redraw));
    let collapsed_again_plain = rendered_plain(app.transcript_lines(20));
    assert!(collapsed_again_plain.contains("Ctrl-T expand"));
    assert!(!collapsed_again_plain.contains("planning step 5"));
    Ok(())
}

#[test]
fn mouse_single_line_thinking_block_has_no_toggle_target() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.handle(RunEvent::ReasoningDelta("single visible step".to_owned()))?;
    app.handle(RunEvent::ToolCallStarted(ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    }))?;

    let plain = rendered_plain(app.transcript_lines(20));
    assert!(plain.contains("1 line"));
    assert!(!plain.contains("1 line hidden"));
    assert!(!plain.contains("Ctrl-T expand"));
    assert!(plain.contains("single visible step"));

    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);

    assert_eq!(layout.thinking_blocks.len(), 0);
    Ok(())
}

#[test]
fn mouse_short_thinking_block_has_no_toggle_target() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.handle(RunEvent::ReasoningDelta(
        "planning step 1\nplanning step 2\nplanning step 3\nplanning step 4".to_owned(),
    ))?;
    app.handle(RunEvent::ToolCallStarted(ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    }))?;

    let plain = rendered_plain(app.transcript_lines(20));
    assert!(plain.contains("4 lines"));
    assert!(plain.contains("planning step 4"));
    assert!(!plain.contains("hidden"));
    assert!(!plain.contains("Ctrl-T expand"));

    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);

    assert_eq!(layout.thinking_blocks.len(), 0);
    Ok(())
}

#[test]
fn mouse_stale_thinking_block_hit_without_collapsed_content_is_noop() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.handle(RunEvent::ReasoningDelta(
        "planning step 1\nplanning step 2\nplanning step 3\nplanning step 4\nplanning step 5"
            .to_owned(),
    ))?;
    app.handle(RunEvent::ToolCallStarted(ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    }))?;
    let thinking_entry_index = app.collapsible_thinking_entry_indices()[0];
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (column, row) = thinking_block_body_point(&layout, thinking_entry_index);
    app.timeline[thinking_entry_index].text = " \n ".to_owned();

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Noop));
    assert_eq!(
        app.mouse_hover_target,
        Some(HitTarget::ThinkingBlock {
            entry_index: thinking_entry_index
        })
    );
    Ok(())
}

#[test]
fn mouse_click_tool_card_without_text_hit_clears_selection() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.push_timeline(TimelineRole::User, "selected text");
    push_sample_tool_cards(&mut app);
    let mut layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (text_column, text_row) = live_text_point_at_text_offset(&app, &layout, "selected text", 0);
    let (text_end_column, text_end_row) =
        live_text_point_at_text_offset(&app, &layout, "selected text", 20);
    let _ = app.handle_mouse_event(
        mouse(MouseInputKind::LeftDown, text_column, text_row),
        &layout,
    )?;
    let _ = app.handle_mouse_event(
        mouse(MouseInputKind::Drag, text_end_column, text_end_row),
        &layout,
    )?;
    assert!(app.selected_timeline_text().is_some());

    layout.live_text_rows.clear();
    let first_entry_index = app.tool_activity_entry_indices()[0];
    let (column, row) = tool_card_body_point(&layout, first_entry_index);
    let outcome = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert!(app.selected_timeline_text().is_none());
    Ok(())
}

#[test]
fn mouse_click_regular_slash_candidate_executes() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.composer.input = "/".to_owned();
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (column, row) = slash_candidate_point_by_label(&app, &layout, "/config");

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert_eq!(app.active_pane, PaneFocus::Composer);
    assert!(app.is_config_mode());
    assert!(app.composer.input.is_empty());
    Ok(())
}

#[test]
fn mouse_release_only_slash_candidate_executes_without_prior_down() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.composer.input = "/".to_owned();
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (column, row) = slash_candidate_point_by_label(&app, &layout, "/config");

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::LeftUp, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert_eq!(app.active_pane, PaneFocus::Composer);
    assert!(app.is_config_mode());
    assert!(app.composer.input.is_empty());
    Ok(())
}

#[test]
fn mouse_down_then_release_slash_candidate_does_not_execute_release_again() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.composer.input = "/".to_owned();
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (column, row) = slash_candidate_point_by_label(&app, &layout, "/config");

    let down = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;
    let up = app.handle_mouse_event(mouse(MouseInputKind::LeftUp, column, row), &layout)?;

    assert!(matches!(down, AppMouseOutcome::Redraw));
    assert!(matches!(up, AppMouseOutcome::Noop));
    assert!(app.is_config_mode());
    Ok(())
}

#[test]
fn mouse_click_dangerous_slash_candidate_requires_second_click() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.composer.input = "/".to_owned();
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (column, row) = slash_candidate_point_by_label(&app, &layout, "/compact");

    let first = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(first, AppMouseOutcome::Redraw));
    assert_eq!(app.composer.input, "/compact");
    assert_eq!(app.last_notice(), Some("click again to confirm /compact"));

    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (column, row) = slash_candidate_point_by_label(&app, &layout, "/compact");
    let second = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(
        second,
        AppMouseOutcome::Action(AppAction::PreviewV2Compaction)
    ));
    assert!(app.composer.input.is_empty());
    Ok(())
}

#[test]
fn mouse_click_quit_shows_confirmation_in_slash_row() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.composer.input = "/q".to_owned();
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (column, row) = slash_candidate_point_by_label(&app, &layout, "/quit");

    let first = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(first, AppMouseOutcome::Redraw));
    assert!(!app.should_quit);
    assert_eq!(app.composer.input, "/quit");
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
    app.composer.input = "/".to_owned();

    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let (column, row) = slash_candidate_point_by_label(&app, &layout, "/config");

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Noop));
    assert_eq!(app.active_pane, PaneFocus::Activity);
    assert_eq!(app.composer.input, "/");
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
    app.composer.input = "/compact".to_owned();

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(matches!(action, Some(AppAction::PreviewV2Compaction)));
    assert_eq!(app.last_notice(), Some("V2 compact preview requested"));
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
    app.composer.input = "/".to_owned();
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
    assert!(app.approval.scroll_back > 0);
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
    assert_eq!(app.approval.scroll_back, 0);
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
fn mouse_click_info_rail_agent_row_switches_visible_agent() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(140, 32);
    app.active_pane = PaneFocus::Composer;
    app.sync_current_session_state(child_agent_entries(
        Some("仓库审查"),
        sigil_kernel::AgentThreadStatus::Completed,
        sigil_kernel::SessionRef::new_relative("children/task_1/step_1-child_1.jsonl")?,
    )?);
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 140, 32), &app);
    let child_row = layout.info_rail_agent_rows[1];
    assert_eq!(
        layout.hit_target(child_row.area.x, child_row.area.y),
        HitTarget::InfoRailAgentRow { index: 1 }
    );

    let outcome = app.handle_mouse_event(
        mouse(MouseInputKind::LeftDown, child_row.area.x, child_row.area.y),
        &layout,
    )?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert_eq!(app.active_pane, PaneFocus::Activity);
    assert!(!app.is_composer_agent_panel_focused());
    assert_eq!(app.active_agent_label(), "仓库审查");
    assert_eq!(
        app.last_notice(),
        Some(
            "agent focus: 仓库审查 · completed · subagent_read · background task · deepseek-v4-pro · tools scoped · workspace inherited · result missing"
        )
    );

    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 140, 32), &app);
    let main_row = layout.info_rail_agent_rows[0];
    let outcome = app.handle_mouse_event(
        mouse(MouseInputKind::LeftDown, main_row.area.x, main_row.area.y),
        &layout,
    )?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert_eq!(app.active_agent_label(), "main");
    Ok(())
}

#[test]
fn mouse_agent_row_left_up_and_scroll_cover_fallback_edges() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.active_pane = PaneFocus::Composer;
    let mut invalid_layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let unavailable_row = invalid_layout.info_rail_agent_rows[0];
    invalid_layout.info_rail_agent_rows[0].index = 99;

    let unavailable = app.handle_mouse_event(
        mouse(
            MouseInputKind::LeftUp,
            unavailable_row.area.x,
            unavailable_row.area.y,
        ),
        &invalid_layout,
    )?;

    assert!(matches!(unavailable, AppMouseOutcome::Noop));
    assert_eq!(app.active_pane, PaneFocus::Activity);
    assert_eq!(app.last_notice(), Some("no agent selected"));

    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let main_row = layout.info_rail_agent_rows[0];
    let scrolled = app.handle_mouse_event(
        mouse(MouseInputKind::ScrollDown, main_row.area.x, main_row.area.y),
        &layout,
    )?;

    assert!(matches!(scrolled, AppMouseOutcome::Redraw));
    assert_eq!(app.active_pane, PaneFocus::Activity);
    assert_eq!(app.sidebar_selected_card.label(), "agents");
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
fn mouse_tool_card_anchor_edges_ignore_stale_state() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    push_sample_tool_cards(&mut app);
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let indices = app.tool_activity_entry_indices();
    let (first_column, first_row) = tool_card_header_point(&layout, indices[0]);

    assert!(
        app.tool_card_mouse_anchor(indices[1], first_column, first_row, &layout)
            .is_none()
    );

    let anchor = super::super::mouse_flow::ToolCardMouseAnchor {
        entry_line_offset: 0,
        viewport_line_offset: 0,
        visible_rows: 1,
    };
    app.restore_tool_card_mouse_anchor(indices[0], None);
    app.restore_tool_card_mouse_anchor(usize::MAX, Some(anchor));
    app.timeline.clear();
    app.rebuild_timeline_render_store();
    app.restore_tool_card_mouse_anchor(indices[0], Some(anchor));
    Ok(())
}

#[test]
fn mouse_scroll_approval_modal_hit_when_no_pending_is_noop() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(80, 20);
    let layout = LayoutSnapshot {
        verification_card: None,
        screen: Rect::new(0, 0, 80, 20),
        mode: LayoutMode::Main,
        live_panel: Rect::new(0, 0, 80, 12),
        egress_disclosure: None,
        composer: Rect::new(0, 12, 80, 4),
        agent_panel: Rect::default(),
        composer_input: Rect::default(),
        footer: Rect::new(0, 16, 80, 4),
        info_rail: Rect::new(60, 0, 20, 12),
        live_text_rows: Vec::new(),
        tool_cards: Vec::new(),
        thinking_blocks: Vec::new(),
        info_rail_agent_rows: Vec::new(),
        slash_overlay: None,
        approval_modal: Some(Rect::new(10, 2, 20, 6)),
        approval_modal_hit_areas: None,
        setup_hit_areas: None,
        config_hit_areas: None,
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
        verification_card: None,
        screen: Rect::new(0, 0, 80, 20),
        mode: LayoutMode::Main,
        live_panel: Rect::new(0, 0, 80, 12),
        egress_disclosure: None,
        composer: Rect::new(0, 12, 80, 4),
        agent_panel: Rect::default(),
        composer_input: Rect::default(),
        footer: Rect::new(0, 16, 80, 4),
        info_rail: Rect::new(60, 0, 20, 12),
        live_text_rows: Vec::new(),
        tool_cards: Vec::new(),
        thinking_blocks: Vec::new(),
        info_rail_agent_rows: Vec::new(),
        slash_overlay: None,
        approval_modal: None,
        approval_modal_hit_areas: None,
        setup_hit_areas: None,
        config_hit_areas: None,
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
fn mouse_scroll_uses_terminal_scroll_sensitivity() -> Result<()> {
    let mut config = test_config();
    config.terminal.scroll_sensitivity = 5;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.set_terminal_size(80, 12);
    for index in 0..8 {
        app.push_timeline(TimelineRole::Assistant, format!("message {index}"));
    }
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 80, 12), &app);
    let (column, row) = point_in(layout.live_panel);

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::ScrollUp, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert_eq!(app.timeline_scroll_back, 5);
    Ok(())
}

#[test]
fn mouse_scroll_approval_with_pending_approval_scrolls_upward() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    inject_write_file_approval(&mut app, sample_approval_preview())?;
    app.approval.scroll_back = 5;
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let approval = layout.approval_modal.expect("expected approval area");
    let (column, row) = point_in(approval);

    let outcome = app.handle_mouse_event(mouse(MouseInputKind::ScrollUp, column, row), &layout)?;

    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert_eq!(app.approval.scroll_back, 2);
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
    assert_eq!(app.approval.selected_file_index, 1);
    assert_eq!(app.approval.selected_hunk_index, 0);
    assert_eq!(app.approval.scroll_back, 0);
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

    app.approval
        .pending
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
    assert_eq!(app.approval.scroll_back, 3);
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
    assert_eq!(app.approval.selected_hunk_index, 1);
    assert!(app.approval.scroll_back > 0);

    let (column, row) = point_in(hit_areas.hunk_previous);
    let hunk_previous =
        app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;
    assert!(matches!(hunk_previous, AppMouseOutcome::Redraw));
    assert_eq!(app.approval.selected_hunk_index, 0);

    let (column, row) = point_in(hit_areas.diff_view_toggle);
    let view_toggle =
        app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;
    assert!(matches!(view_toggle, AppMouseOutcome::Redraw));
    assert_eq!(app.approval.diff_mode.label(), "current-hunk");
    assert_eq!(app.approval.scroll_back, 0);

    let (column, row) = point_in(hit_areas.metadata_toggle);
    let metadata_toggle =
        app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;
    assert!(matches!(metadata_toggle, AppMouseOutcome::Redraw));
    assert!(app.approval.metadata_collapsed);
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
    assert_eq!(app.approval.selected_hunk_index, 0);
    assert_eq!(app.approval.scroll_back, 0);

    let (column, row) = point_in(hit_areas.hunk_next);
    let next = app.handle_mouse_event(mouse(MouseInputKind::LeftDown, column, row), &layout)?;

    assert!(matches!(next, AppMouseOutcome::Noop));
    assert_eq!(app.approval.selected_hunk_index, 0);
    assert_eq!(app.approval.scroll_back, 0);
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
        .allow_once_action;
    app.approval.pending = None;
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
        .allow_once_action;
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
