use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, style::Color};
use serde_json::json;
use sigil_kernel::{
    AgentConfig, AgentRole, CheckCommand, CheckDiscoverySource, CheckPromotion, CheckSpec,
    CheckSpecRecordedEntry, CompactionConfig, ControlEntry, EventHandler, EvidenceScope,
    JsonlSessionStore, MemoryConfig, ModelMessage, PermissionConfig, ReadinessEvaluatedEntry,
    ReadinessEvaluation, RequiredAction, RootConfig, RunEvent, RunStatus, SessionConfig,
    SessionLogEntry, SessionRef, TaskId, TaskPlanEntry, TaskPlanStatus, TaskRunEntry,
    TaskRunStatus, TaskStepEntry, TaskStepId, TaskStepSpec, TaskStepStatus, ToolAccess, ToolCall,
    ToolCategory, ToolEffect, ToolPreview, ToolPreviewCapability, ToolPreviewFile,
    ToolPreviewSnapshot, ToolResult, ToolResultMeta, ToolSpec, TrustedCheckSpec,
    VerificationVerdict, VisibleCompletionState, WorkspaceConfig,
};
use tempfile::tempdir;

use crate::app::AppState;
use crate::config_panel::ConfigSection;
use crate::runner::{
    V2CompactionAdmission, V2CompactionPreviewState, V2CompactionReview, WorkerMessage,
};
use crate::timeline::RunPhase;

use super::super::theme::{
    Theme, config_border, config_primary, config_section_bg, config_selected_bg, config_tab_bg,
};
use super::*;

static TEST_STORAGE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn test_config() -> RootConfig {
    let storage_id = TEST_STORAGE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let storage_root = std::env::temp_dir().join(format!(
        "sigil-tui-shell-test-storage-{}-{storage_id}",
        std::process::id()
    ));
    RootConfig {
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        storage: sigil_kernel::StorageConfig {
            state_root: sigil_kernel::StorageRoot::Path(
                storage_root.join("state").display().to_string(),
            ),
            cache_root: sigil_kernel::StorageRoot::Path(
                storage_root.join("cache").display().to_string(),
            ),
            ..Default::default()
        },
        session: SessionConfig::default(),
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        model_request: Default::default(),
        permission: PermissionConfig::default(),
        memory: MemoryConfig { enabled: true },
        skills: Default::default(),
        compaction: CompactionConfig::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: Default::default(),
        providers: BTreeMap::new(),
        web: Default::default(),
        mcp_servers: Vec::new(),
    }
}

fn open_config_panel_for_test(app: &mut AppState) -> anyhow::Result<()> {
    app.composer.input = "/config".to_owned();
    let _ = app.submit_input()?;
    Ok(())
}

fn select_config_section_for_test(app: &mut AppState, section: ConfigSection) {
    app.select_config_section_for_test(section);
}

fn write_plugin_with_many_capabilities(workspace_root: &Path) -> anyhow::Result<()> {
    let plugin_root = workspace_root.join(".sigil/plugins/command-pack");
    fs::create_dir_all(&plugin_root)?;
    fs::write(
        plugin_root.join("plugin.toml"),
        r#"id = "command-pack"
name = "Command Pack"
version = "0.1.0"

[[hooks]]
event = "pre_tool_use"
command = "scripts/hook-1.sh"
args = ["--flag-1"]
approval = "ask"

[[hooks]]
event = "post_tool_use"
command = "scripts/hook-2.sh"
args = ["--flag-2"]
approval = "ask"

[[hooks]]
event = "session_start"
command = "scripts/hook-3.sh"
args = ["--flag-3"]
approval = "deny"

[[hooks]]
event = "session_stop"
command = "scripts/hook-4.sh"
args = ["--flag-4"]
approval = "allow"

[[mcp_servers]]
name = "tools-1"
transport = "stdio"
command = "node"
args = ["server-1.js"]
startup = "lazy"
required = false

[[mcp_servers]]
name = "tools-2"
transport = "stdio"
command = "node"
args = ["server-2.js"]
startup = "lazy"
required = false

[[mcp_servers]]
name = "tools-3"
transport = "stdio"
command = "node"
args = ["server-3.js"]
startup = "eager"
required = true

[[mcp_servers]]
name = "tools-4"
transport = "stdio"
command = "node"
args = ["server-4.js"]
startup = "eager"
required = true
"#,
    )?;
    Ok(())
}

#[test]
fn render_main_screen_shows_keyboard_help_modal() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE))?;
    let backend = TestBackend::new(112, 42);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Session"));
    assert!(rendered.contains("Review"));
    assert!(rendered.contains("Navigation"));
    Ok(())
}

#[test]
fn render_main_screen_shows_feedback_privacy_preview_modal() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input = "/feedback".to_owned();
    assert!(app.submit_input()?.is_none());
    let backend = TestBackend::new(140, 42);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("Feedback Report"));
    assert!(rendered.contains("Review before sharing"));
    assert!(rendered.contains("Excluded: conversation"));
    assert!(rendered.contains("Enter export locally"));
    Ok(())
}

#[test]
fn render_main_screen_keeps_error_notice_visible() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle(RunEvent::Notice(
        "permission read_file failed: crates/sigil-kernel/src/skill.rs denied".to_owned(),
    ))?;
    let backend = TestBackend::new(120, 24);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("error"));
    assert!(rendered.contains("permission read_file failed"));
    assert!(!rendered.contains("notice info"));
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
    assert!(!rendered.contains("Esc interrupt"));
    assert!(rendered.contains("reasoning with deepseek-v4-flash"));
    Ok(())
}

#[test]
fn render_config_screen_uses_details_side_panel_on_wide_terminals() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let backend = TestBackend::new(160, 36);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("Config"));
    assert!(rendered.contains("Details"));
    assert!(rendered.contains("Provider 1/13"));
    assert!(rendered.contains("▸ Model"));
    assert!(rendered.contains("key model"));
    assert!(rendered.contains("keys Tab section"));
    assert!(rendered.contains("actions Down to actions"));
    assert!(rendered.contains("✓ saved"));
    assert!(!rendered.contains("Status"));
    assert!(!rendered.contains("Actions"));
    assert!(!rendered.contains("provider settings · Tab"));
    assert!(!rendered.contains("[details]"));
    Ok(())
}

#[test]
fn render_config_mcp_selector_keeps_lifecycle_visible_at_120x32() -> anyhow::Result<()> {
    let mut config = test_config();
    for index in 0..8 {
        config.mcp_servers.push(mcp_server_config! {
            name: format!("mcp-{index}"),
            command: "mcp-probe".to_owned(),
            startup: sigil_kernel::McpServerStartup::Lazy,
            ..Default::default()
        });
    }
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    open_config_panel_for_test(&mut app)?;
    select_config_section_for_test(&mut app, ConfigSection::Mcp);
    let backend = TestBackend::new(120, 32);
    let mut terminal = Terminal::new(backend)?;

    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    terminal.draw(|frame| render(frame, &app))?;
    let front = rendered_content(&terminal);
    assert!(front.contains("mcp-1"));
    assert!(front.contains("Live fingerprint"));
    assert!(front.contains("Secrets"));
    assert!(front.contains("Boundary"));

    for _ in 0..5 {
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    }
    terminal.draw(|frame| render(frame, &app))?;
    let back = rendered_content(&terminal);
    assert!(back.contains("mcp-6"));
    assert!(back.contains("Live fingerprint"));
    assert!(back.contains("Secrets"));
    assert!(back.contains("Boundary"));
    Ok(())
}

#[test]
fn render_config_storage_paths_use_wider_main_panel_on_wide_terminals() -> anyhow::Result<()> {
    let mut config = test_config();
    config.workspace.root = "/Users/example/study/turbods".to_owned();
    config.storage.state_root = sigil_kernel::StorageRoot::Path(
        "/Users/example/Library/Application Support/sigil/state".to_owned(),
    );
    config.storage.cache_root =
        sigil_kernel::StorageRoot::Path("/Users/example/Library/Caches/sigil".to_owned());
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    open_config_panel_for_test(&mut app)?;
    select_config_section_for_test(&mut app, ConfigSection::Storage);
    let backend = TestBackend::new(220, 36);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rows = rendered_rows(&terminal);
    let input_history_row = rows
        .iter()
        .find(|row| row.contains("Input history"))
        .expect("input history storage row should render");
    assert!(
        input_history_row.contains("input-history.jsonl"),
        "wide storage row should keep the file name visible: {input_history_row}"
    );
    assert!(
        !input_history_row.contains("..."),
        "wide storage row should not truncate the resolved path: {input_history_row}"
    );
    assert!(rendered_content(&terminal).contains("Details"));
    Ok(())
}

#[test]
fn render_main_screen_uses_configured_theme_surface() -> anyhow::Result<()> {
    let mut config = test_config();
    config.appearance.theme = sigil_kernel::ThemeId::SolarizedLight;
    let app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    let backend = TestBackend::new(120, 24);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let expected = Theme::builtin(sigil_kernel::ThemeId::SolarizedLight)
        .palette
        .surface_base;
    let actual = terminal
        .backend()
        .buffer()
        .cell((0, 0))
        .expect("top-left cell should exist")
        .bg;
    assert_eq!(actual, expected);
    assert_ne!(actual, Color::Rgb(7, 8, 10));
    Ok(())
}

#[test]
fn render_main_screen_custom_theme_reaches_timeline_tool_card_and_composer() -> anyhow::Result<()> {
    let mut config = test_config();
    let mut colors = BTreeMap::new();
    colors.insert("surface_base".to_owned(), "#010203".to_owned());
    colors.insert("surface_panel".to_owned(), "#111213".to_owned());
    colors.insert("surface_input".to_owned(), "#212223".to_owned());
    colors.insert("surface_user_message".to_owned(), "#515253".to_owned());
    colors.insert("surface_selection".to_owned(), "#414243".to_owned());
    colors.insert("text_primary".to_owned(), "#F0F1F2".to_owned());
    colors.insert("markdown_code_fg".to_owned(), "#D0D1D2".to_owned());
    colors.insert("markdown_code_bg".to_owned(), "#313233".to_owned());
    config.appearance.colors = sigil_kernel::ThemeColorOverrides::new(colors);
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.set_terminal_size(160, 36);
    app.composer.input = "composer-visible".to_owned();
    app.handle(RunEvent::TextDelta("assistant `inline-code`".to_owned()))?;
    let call = ToolCall {
        id: "call-themed-read".to_owned(),
        name: "read_file".to_owned(),
        args_json: r#"{"path":"Cargo.toml"}"#.to_owned(),
    };
    app.handle(RunEvent::ToolCallStarted(call.clone()))?;
    app.handle(RunEvent::ToolCallCompleted(call.clone()))?;
    let mut meta = ToolResultMeta {
        returned_bytes: Some(64),
        total_bytes: Some(64),
        ..ToolResultMeta::default()
    };
    meta.details = json!({"path":"Cargo.toml"});
    app.handle(RunEvent::ToolResult(ToolResult::ok(
        call.id,
        call.name,
        "# Tool\n`tool-code`",
        meta,
    )))?;
    let backend = TestBackend::new(160, 36);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    assert_eq!(
        terminal
            .backend()
            .buffer()
            .cell((0, 0))
            .expect("top-left cell should exist")
            .bg,
        Color::Rgb(1, 2, 3)
    );
    assert_eq!(
        cell_bg_at_text(&terminal, "composer-visible", "composer-visible"),
        Color::Rgb(33, 34, 35)
    );
    assert_eq!(
        cell_bg_at_text(&terminal, "inline-code", "inline-code"),
        Color::Rgb(49, 50, 51)
    );
    assert_eq!(
        cell_bg_at_text(&terminal, "tool-code", "tool-code"),
        Color::Rgb(65, 66, 67)
    );
    Ok(())
}

#[test]
fn render_config_theme_draft_previews_immediately() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    open_config_panel_for_test(&mut app)?;
    select_config_section_for_test(&mut app, ConfigSection::Appearance);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let backend = TestBackend::new(120, 28);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let expected_theme = Theme::builtin(sigil_kernel::ThemeId::SigilDark.next());
    let expected = expected_theme.palette.config_bg;
    let actual = terminal
        .backend()
        .buffer()
        .cell((0, 0))
        .expect("top-left cell should exist")
        .bg;
    assert_eq!(actual, expected);
    assert_eq!(
        cell_fg_at_text(&terminal, "Appearance 9/13", "Appearance"),
        expected_theme.palette.text_primary
    );
    assert_eq!(
        cell_fg_at_text(&terminal, "Built-ins", "Built-ins"),
        expected_theme.palette.text_secondary
    );
    assert_ne!(
        cell_fg_at_text(&terminal, "Appearance 9/13", "Appearance"),
        Theme::builtin(sigil_kernel::ThemeId::SigilDark)
            .palette
            .text_primary
    );
    assert_eq!(
        app.root_config_snapshot()
            .expect("config snapshot should exist")
            .appearance
            .theme,
        sigil_kernel::ThemeId::SigilDark
    );
    Ok(())
}

#[test]
fn render_config_saved_theme_uses_theme_text_palette() -> anyhow::Result<()> {
    let mut config = test_config();
    config.appearance.theme = sigil_kernel::ThemeId::SolarizedLight;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.composer.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let backend = TestBackend::new(120, 28);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let expected = Theme::builtin(sigil_kernel::ThemeId::SolarizedLight);
    assert_eq!(
        terminal
            .backend()
            .buffer()
            .cell((0, 0))
            .expect("top-left cell should exist")
            .bg,
        expected.palette.config_bg
    );
    assert_eq!(
        cell_fg_at_text(&terminal, "Provider 1/13", "Provider"),
        expected.palette.text_primary
    );
    assert_eq!(
        cell_fg_at_text(&terminal, "file sigil.toml", "sigil.toml"),
        expected.palette.text_secondary
    );
    Ok(())
}

#[test]
fn render_config_custom_color_override_updates_preview_surface() -> anyhow::Result<()> {
    let mut config = test_config();
    let mut colors = BTreeMap::new();
    colors.insert("config_bg".to_owned(), "#010203".to_owned());
    colors.insert("text_primary".to_owned(), "#F0F1F2".to_owned());
    config.appearance.colors = sigil_kernel::ThemeColorOverrides::new(colors);
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    open_config_panel_for_test(&mut app)?;
    select_config_section_for_test(&mut app, ConfigSection::Appearance);
    assert_eq!(
        app.config_selected_section(),
        Some(ConfigSection::Appearance)
    );
    let backend = TestBackend::new(128, 80);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    assert_eq!(
        terminal
            .backend()
            .buffer()
            .cell((0, 0))
            .expect("top-left cell should exist")
            .bg,
        Color::Rgb(1, 2, 3)
    );
    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("preview page"));
    assert!(rendered.contains("Overrides"));
    assert!(rendered.contains("Fine-grained color token overrides are edited in sigil.toml"));
    Ok(())
}

#[test]
fn render_config_common_widths_keep_core_structure() -> anyhow::Result<()> {
    for width in [80, 96, 160] {
        for (right_presses, title, selected) in [
            (0, "Provider 1/13", "▸ Model"),
            (3, "Memory 5/13", "▸ Memory"),
            (4, "Compaction 6/13", "▸ Auto compact"),
        ] {
            let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
            app.composer.input = "/config".to_owned();
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
fn render_config_step_tabs_keep_selected_section_visible_on_narrow_width() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    open_config_panel_for_test(&mut app)?;
    select_config_section_for_test(&mut app, ConfigSection::Plugins);
    let backend = TestBackend::new(80, 28);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rows = rendered_rows(&terminal);
    let step_row = rows
        .iter()
        .find(|row| row.contains("plugins") && row.contains("mcp"))
        .expect("current section should remain visible in the section tabs");
    assert!(step_row.contains("..."));
    assert!(!step_row.contains("provider"));
    Ok(())
}

#[test]
fn render_config_centers_content_on_very_wide_terminals() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input = "/config".to_owned();
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
    app.composer.input = "/config".to_owned();
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
    app.composer.input = "/config".to_owned();
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
    app.composer.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let backend = TestBackend::new(160, 36);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rows = rendered_rows(&terminal);
    let footer_y = rows
        .iter()
        .position(|row| row.contains("[save]") && row.contains("✓ saved"))
        .expect("config footer toolbar should render");
    let footer_row = &rows[footer_y];
    let close_x = char_index_of(footer_row, "[close]").expect("close action should render");
    let status_x = char_index_of(footer_row, "✓ saved").expect("status should render");
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
    assert!(footer_row.trim_end().ends_with("✓ saved"));
    assert!(chip_bg_cells > 20);
    Ok(())
}

#[test]
fn render_config_screen_uses_muted_palette_instead_of_terminal_green() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input = "/config".to_owned();
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
    app.composer.input = "/config".to_owned();
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
    app.composer.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let backend = TestBackend::new(132, 36);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rows = rendered_rows(&terminal);
    let value_columns = ["Model", "API key", "Provider"]
        .into_iter()
        .map(|label| {
            rows.iter()
                .find(|row| {
                    row.contains(label)
                        && row.contains(':')
                        && !row.contains("field:")
                        && !row.contains("▸ ")
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
    app.composer.input = "/config".to_owned();
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
    terminal.draw(|frame| render(frame, &app))?;
    let rows = rendered_rows(&terminal);
    let api_key_action_x = rows
        .iter()
        .find(|row| row.contains("API key") && row.contains("[input]"))
        .and_then(|row| char_index_of(row, "[input]"))
        .expect("api key action chip should render");
    assert!(
        !rows.iter().any(|row| row.contains("[Enter input]")),
        "main config form should keep shortcut text out of action chips"
    );

    assert_eq!(
        model_action_x, api_key_action_x,
        "config action chips should share a stable action column"
    );
    Ok(())
}

#[test]
fn render_config_readonly_rows_align_value_column() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    open_config_panel_for_test(&mut app)?;
    select_config_section_for_test(&mut app, ConfigSection::Memory);
    let backend = TestBackend::new(132, 36);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rows = rendered_rows(&terminal);
    let documents_row = rows
        .iter()
        .position(|row| row.contains("◇ Documents"))
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
    assert_eq!(readonly_chip_cells, 0);
    Ok(())
}

#[test]
fn shell_footer_helpers_cover_context_thresholds() {
    let footer = FooterViewModel {
        run_label: "Run".to_owned(),
        hints: "hint".to_owned(),
        is_busy: false,
        phase: RunPhase::Idle,
        context_label: "ctx 42%".to_owned(),
    };

    assert_eq!(footer_context_width(&footer, 20), 0);
    assert_eq!(footer_context_width(&footer, 40), 7);
}

#[test]
fn shell_path_and_memory_badges_cover_fallback_states() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.runtime.memory_enabled = false;
    assert_eq!(memory_badge(&app), "off");

    app.runtime.memory_enabled = true;
    app.runtime.memory_document_count = 3;
    app.runtime.memory_last_status = "ok".to_owned();
    assert_eq!(memory_badge(&app), "3/ok");

    app.runtime.memory_last_status = "failed".to_owned();
    assert_eq!(memory_badge(&app), "3/err");

    assert_eq!(short_path_label(Path::new("/tmp/demo")), "demo");
    assert_eq!(short_path_label(Path::new(".")), ".");
    assert_eq!(short_session_id("1234567890"), "12345678");
    assert_eq!(short_pane_label(&app), "composer");
    Ok(())
}

#[test]
fn render_status_setup_mode_uses_setup_copy() -> anyhow::Result<()> {
    let app = AppState::from_setup(
        Path::new("sigil.toml").to_path_buf(),
        Path::new(".").to_path_buf(),
        None,
    );
    let backend = TestBackend::new(80, 4);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_status(frame, frame.area(), &app))?;
    let setup_rendered = rendered_content(&terminal);
    assert!(setup_rendered.contains("Sigil setup"));
    assert!(setup_rendered.contains("quick setup"));
    Ok(())
}

#[test]
fn render_status_workspace_trust_mode_uses_trust_copy() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.enter_workspace_trust_gate()?;
    let backend = TestBackend::new(100, 4);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_status(frame, frame.area(), &app))?;
    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("Workspace trust"));
    assert!(rendered.contains("review workspace"));
    Ok(())
}

#[test]
fn render_config_details_panel_uses_focus_row_and_command_tokens() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let backend = TestBackend::new(160, 36);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rows = rendered_rows(&terminal);
    let selected_detail_row = rows
        .iter()
        .position(|row| row.contains("▸ Model"))
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
        .position(|row| row.contains("i Chat model used"))
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
    app.composer.input = "/config".to_owned();
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
    open_config_panel_for_test(&mut app)?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let backend = TestBackend::new(160, 36);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("API Key"));
    assert!(rendered.contains("key: api_key"));
    assert!(rendered.contains("SIGIL_API_KEY can override"));
    assert!(rendered.contains("api_key: |"));
    Ok(())
}

#[test]
fn render_config_text_modal_uses_focus_input_row_and_command_tokens() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    open_config_panel_for_test(&mut app)?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let backend = TestBackend::new(160, 36);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rows = rendered_rows(&terminal);
    let input_row = rows
        .iter()
        .position(|row| row.contains("api_key: |"))
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

    assert!(input_bg_cells > 5);
    assert_eq!(command_token_cells, "EnterF2F3Esc".chars().count());
    Ok(())
}

#[test]
fn render_config_model_picker_uses_config_palette() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input = "/config".to_owned();
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
    app.composer.input = "/config".to_owned();
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
    app.composer.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let backend = TestBackend::new(96, 28);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("Config"));
    assert!(rendered.contains("details"));
    assert!(!rendered.contains("Details"));
    assert!(rendered.contains("▸ Model"));
    Ok(())
}

#[test]
fn render_config_header_truncates_long_status_summary() -> anyhow::Result<()> {
    let long_config_name = "sigil-config-file-name-with-a-very-very-long-project-suffix.toml";
    let mut app = AppState::from_root_config(Path::new(long_config_name), &test_config());
    app.composer.input = "/config".to_owned();
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
    app.composer.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let backend = TestBackend::new(48, 32);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rows = rendered_rows(&terminal);
    let selected_detail_row = rows
        .iter()
        .position(|row| row.contains("▸ Model"))
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
    app.composer.input = "/config".to_owned();
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
fn render_config_plugins_keeps_hook_summary_and_mcp_details_visible_on_narrow_screen()
-> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    write_plugin_with_many_capabilities(&workspace)?;
    let mut config = test_config();
    config.workspace.root = workspace.display().to_string();
    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);
    open_config_panel_for_test(&mut app)?;
    select_config_section_for_test(&mut app, ConfigSection::Plugins);
    let backend = TestBackend::new(96, 80);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("Plugins 12/13"));
    assert!(rendered.contains("Hook count"));
    assert!(rendered.contains(": 4"));
    assert!(rendered.contains("Hook kinds"));
    assert!(rendered.contains("event=4"));
    assert!(rendered.contains("Hook effects"));
    assert!(rendered.contains("unknown=4"));
    assert!(rendered.contains("Inspect"));
    assert!(rendered.contains("run /doctor for command and issue details"));
    assert!(!rendered.contains("scripts/hook-4.sh --flag-4"));
    assert!(rendered.contains("MCP 4"));
    assert!(rendered.contains("tools-4"));
    assert!(rendered.contains("node server-4.js"));
    assert!(!rendered.contains("- Hooks:"));
    assert!(!rendered.contains("- MCP:"));
    Ok(())
}

#[test]
fn render_config_short_terminal_scrolls_to_selected_field() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    open_config_panel_for_test(&mut app)?;
    select_config_section_for_test(&mut app, ConfigSection::Compaction);
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
    app.composer.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let backend = TestBackend::new(132, 30);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("△ unsaved - save before close"));
    assert!(rendered.contains("[save]"));
    assert!(rendered.contains("[close]"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    terminal.draw(|frame| render(frame, &app))?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("✕ confirm"));
    assert!(rendered.contains("> save <"));
    Ok(())
}

#[test]
fn render_config_footer_compacts_on_narrow_terminals() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input = "/config".to_owned();
    let _ = app.submit_input()?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    let backend = TestBackend::new(64, 24);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("> save <"));
    assert!(rendered.contains("[close]"));
    assert!(rendered.contains("..."));
    assert!(rendered.contains("✕ confirm"));
    Ok(())
}

#[test]
fn render_config_screen_marks_readonly_and_hint_rows() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    open_config_panel_for_test(&mut app)?;
    select_config_section_for_test(&mut app, ConfigSection::Memory);
    let backend = TestBackend::new(132, 30);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("Memory 5/13"));
    assert!(rendered.contains("◇ Documents"));
    assert!(rendered.contains("Last scan"));
    assert!(rendered.contains("Root files"));
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

fn cell_fg_at_text(terminal: &Terminal<TestBackend>, row_needle: &str, text: &str) -> Color {
    let rows = rendered_rows(terminal);
    let row_index = rows
        .iter()
        .position(|row| row.contains(row_needle))
        .expect("row should render");
    let column_index = char_index_of(&rows[row_index], text).expect("text should render in row");
    terminal
        .backend()
        .buffer()
        .cell((column_index as u16, row_index as u16))
        .expect("cell in bounds")
        .fg
}

fn cell_bg_at_text(terminal: &Terminal<TestBackend>, row_needle: &str, text: &str) -> Color {
    let rows = rendered_rows(terminal);
    let row_index = rows
        .iter()
        .position(|row| row.contains(row_needle))
        .expect("row should render");
    let column_index = char_index_of(&rows[row_index], text).expect("text should render in row");
    terminal
        .backend()
        .buffer()
        .cell((column_index as u16, row_index as u16))
        .expect("cell in bounds")
        .bg
}

#[test]
fn render_main_screen_shows_pending_approval_overlay() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    inject_write_file_approval(&mut app)?;
    let backend = TestBackend::new(140, 28);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("Review Tool Call"));
    assert!(rendered.contains("Update note.txt"));
    assert!(rendered.contains("Allow"));
    assert!(rendered.contains("Deny"));
    Ok(())
}

#[test]
fn render_setup_screen_shows_workspace_notice_and_panel() -> anyhow::Result<()> {
    let app = AppState::from_setup(
        Path::new("sigil.toml").to_path_buf(),
        Path::new("/tmp/example-workspace").to_path_buf(),
        Some("set auth then save".to_owned()),
    );
    let backend = TestBackend::new(96, 24);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("Sigil setup"));
    assert!(rendered.contains("Quick setup"));
    assert!(rendered.contains("ws=example-workspace"));
    assert!(rendered.contains("cfg=sigil.toml"));
    assert!(rendered.contains("set auth then save"));
    Ok(())
}

#[test]
fn footer_context_width_covers_empty_and_bounded_states() {
    let footer = FooterViewModel {
        phase: RunPhase::Idle,
        is_busy: false,
        run_label: "ready".to_owned(),
        hints: "Enter send".to_owned(),
        context_label: "ctx 12%".to_owned(),
    };

    assert_eq!(footer_context_width(&footer, 20), 0);
    assert_eq!(footer_context_width(&footer, 24), 7);
    assert_eq!(
        footer_context_width(
            &FooterViewModel {
                context_label: "context ".repeat(12),
                ..footer.clone()
            },
            160,
        ),
        42
    );
}

#[test]
fn short_label_helpers_cover_path_session_pane_and_memory_states() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    assert_eq!(short_path_label(Path::new("/tmp/project")), "project");
    assert_eq!(short_path_label(Path::new("/")), "/");
    assert_eq!(short_session_id("1234567890"), "12345678");
    assert_eq!(short_pane_label(&app), "composer");

    app.active_pane = PaneFocus::Activity;
    assert_eq!(short_pane_label(&app), "activity");

    app.runtime.memory_enabled = false;
    assert_eq!(memory_badge(&app), "off");
    app.runtime.memory_enabled = true;
    app.runtime.memory_document_count = 7;
    app.runtime.memory_last_status = "ok".to_owned();
    assert_eq!(memory_badge(&app), "7/ok");
    app.runtime.memory_last_status = "failed".to_owned();
    assert_eq!(memory_badge(&app), "7/err");
}

#[test]
fn render_status_runtime_mode_shows_provider_session_and_runtime_state() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.runtime.is_busy = true;
    app.active_pane = PaneFocus::Activity;
    app.runtime.memory_enabled = false;
    app.runtime.compaction_status = "pending".to_owned();
    app.session_id = "1234567890abcdef".to_owned();
    let backend = TestBackend::new(100, 4);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_status(frame, Rect::new(0, 0, 100, 4), &app))?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("Sigil TUI"));
    assert!(rendered.contains("deepseek/deepseek-v4-flash"));
    assert!(rendered.contains("running"));
    assert!(rendered.contains("sid=12345678"));
    assert!(rendered.contains("pane=activity"));
    assert!(rendered.contains("mem=off"));
    assert!(rendered.contains("compact=pending"));
    Ok(())
}

#[test]
fn render_footer_status_returns_early_for_zero_and_tiny_areas() -> anyhow::Result<()> {
    let footer = FooterViewModel {
        phase: RunPhase::Idle,
        is_busy: false,
        run_label: "ready".to_owned(),
        hints: String::new(),
        context_label: "ctx 12%".to_owned(),
    };
    let backend = TestBackend::new(8, 2);
    let mut terminal = Terminal::new(backend)?;
    let theme = theme::Theme::default();

    terminal.draw(|frame| {
        render_footer_status(frame, Rect::new(0, 0, 0, 1), &footer, &theme);
        render_footer_status(frame, Rect::new(0, 1, 3, 1), &footer, &theme);
    })?;

    Ok(())
}

#[test]
fn render_footer_status_omits_context_when_width_is_small_or_label_is_empty() -> anyhow::Result<()>
{
    let footer = FooterViewModel {
        phase: RunPhase::Idle,
        is_busy: false,
        run_label: "ready".to_owned(),
        hints: "Enter send".to_owned(),
        context_label: String::new(),
    };
    let backend = TestBackend::new(24, 2);
    let mut terminal = Terminal::new(backend)?;
    let theme = theme::Theme::default();

    terminal.draw(|frame| render_footer_status(frame, Rect::new(0, 0, 24, 1), &footer, &theme))?;

    let rendered = rendered_content(&terminal);
    assert!(!rendered.contains("ready"));
    assert!(!rendered.contains("Enter send"));
    assert!(!rendered.contains("ctx"));
    Ok(())
}

#[test]
fn render_main_screen_supports_activity_pane_without_cursor_logic() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.active_pane = PaneFocus::Activity;
    let backend = TestBackend::new(120, 24);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("Build"));
    assert!(rendered.contains("ctx"));
    Ok(())
}

#[test]
#[ignore = "generates documentation screenshots under site/assets/screenshots"]
fn render_docs_screenshot_assets() -> anyhow::Result<()> {
    let screenshot_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("site/assets/screenshots");
    fs::create_dir_all(&screenshot_dir)?;

    write_terminal_svg(
        &screenshot_dir.join("tui-session.svg"),
        "Sigil TUI session preview",
        "Generated from the Sigil TUI renderer: transcript, tool activity, composer, and info rail.",
        &mut docs_session_app()?,
    )?;

    let mut approval_app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    approval_app.set_terminal_size(DOC_SCREENSHOT_COLUMNS, DOC_SCREENSHOT_ROWS);
    inject_write_file_approval(&mut approval_app)?;
    write_terminal_svg(
        &screenshot_dir.join("approval-review.svg"),
        "Sigil approval review preview",
        "Generated from the Sigil TUI renderer: approval modal with file diff preview and allow/deny actions.",
        &mut approval_app,
    )?;

    let mut config_app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    config_app.set_terminal_size(DOC_SCREENSHOT_COLUMNS, DOC_SCREENSHOT_ROWS);
    config_app.composer.input = "/config".to_owned();
    let _ = config_app.submit_input()?;
    write_terminal_svg(
        &screenshot_dir.join("config-panel.svg"),
        "Sigil config panel preview",
        "Generated from the Sigil TUI renderer: provider, permissions, memory, compaction, code intelligence, terminal, appearance, Agents, Skills, Plugins trust review, and MCP settings.",
        &mut config_app,
    )?;

    write_terminal_svg(
        &screenshot_dir.join("verification-card.svg"),
        "Sigil task verification preview",
        "Generated from the Sigil TUI renderer: focused Verification card with recommended check and inspectable snapshot evidence.",
        &mut docs_verification_app()?,
    )?;

    write_terminal_svg(
        &screenshot_dir.join("checkpoint-restore.svg"),
        "Sigil checkpoint restore preview",
        "Generated from the Sigil TUI renderer: controlled checkpoint restore review with reverse diff and restore or fork choices.",
        &mut docs_checkpoint_restore_app()?,
    )?;

    write_terminal_svg(
        &screenshot_dir.join("compaction-preview.svg"),
        "Sigil context compaction preview",
        "Generated from the Sigil TUI renderer: read-only Context Compaction V2 review with apply unavailable while admission is frozen.",
        &mut docs_compaction_preview_app()?,
    )?;

    Ok(())
}

fn docs_verification_app() -> anyhow::Result<AppState> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(DOC_SCREENSHOT_COLUMNS, DOC_SCREENSHOT_ROWS);
    app.composer.input = "Verify the documentation refresh".to_owned();
    let _ = app.submit_input()?;
    app.handle(RunEvent::ReasoningDelta(
        "I will run the repository-owned documentation and Pages checks.".to_owned(),
    ))?;
    app.handle(RunEvent::TextDelta(
        "The content update is ready; the required check still needs a recorded receipt."
            .to_owned(),
    ))?;
    app.runtime.is_busy = false;
    app.session_browser.current_entries = docs_verification_entries()?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::ALT))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE))?;
    Ok(app)
}

fn docs_verification_entries() -> anyhow::Result<Vec<SessionLogEntry>> {
    let task_id = TaskId::new("task_1")?;
    let step_id = TaskStepId::new("step_1")?;
    let trusted = TrustedCheckSpec {
        check_spec: CheckSpec::new(
            "cargo-test",
            CheckCommand {
                command: "cargo".to_owned(),
                args: vec!["test".to_owned()],
                cwd: None,
            },
            ToolEffect::ReadOnly,
            "task_step_default",
        ),
        source: CheckDiscoverySource::UserExplicitConfig,
        workspace_trust_snapshot_id: "trust-1".to_owned(),
        promoted_by: CheckPromotion::ExplicitUserConfig {
            config_event_id: "config-verification".to_owned(),
        },
        approval_event_id: None,
        sandbox_decision_id: None,
    };
    Ok(vec![
        SessionLogEntry::User(ModelMessage::user("Verify the documentation refresh")),
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
            objective: "Verify the documentation refresh".to_owned(),
            status: TaskRunStatus::Paused,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: vec![TaskStepSpec {
                step_id: step_id.clone(),
                title: "Run documentation checks".to_owned(),
                display_name: None,
                detail: None,
                role: AgentRole::Executor,
                depends_on: Vec::new(),
                mode: None,
                isolation: None,
            }],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            step_id: step_id.clone(),
            role: AgentRole::Executor,
            status: TaskStepStatus::Blocked,
            title: Some("Run documentation checks".to_owned()),
            summary: None,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::CheckSpecRecorded(
            CheckSpecRecordedEntry::new(
                EvidenceScope::Task(task_id.as_str().to_owned()),
                trusted,
                "config-verification",
            ),
        )),
        SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(ReadinessEvaluatedEntry {
            scope: EvidenceScope::Step(format!("{}:{}", task_id.as_str(), step_id.as_str())),
            evaluation: ReadinessEvaluation {
                run_status: RunStatus::Completed,
                verification_verdict: VerificationVerdict::Missing,
                visible_state: VisibleCompletionState::NeedsUser,
                reasons: Vec::new(),
                required_actions: vec![RequiredAction::RunCheck {
                    check_spec_id: "cargo-test".to_owned(),
                }],
            },
            policy_hash: Some("policy-hash".to_owned()),
            workspace_snapshot_id: Some("snapshot-docs-1".to_owned()),
        })),
    ])
}

fn docs_checkpoint_restore_app() -> anyhow::Result<AppState> {
    let temp = tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let note = workspace.join("release-notes.md");
    fs::write(&note, "alpha\nbeta\n")?;
    let config = RootConfig {
        workspace: WorkspaceConfig {
            root: workspace.display().to_string(),
        },
        ..test_config()
    };
    let mut app = AppState::from_root_config(temp.path().join("sigil.toml").as_path(), &config);
    let session_path = temp.path().join("checkpoint-preview.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    store.append(&SessionLogEntry::User(ModelMessage::user(
        "Update the release notes",
    )))?;
    let preview = ToolPreview {
        title: "Update release-notes.md".to_owned(),
        summary: "Refresh the release boundary".to_owned(),
        body: "--- current/release-notes.md\n+++ proposed/release-notes.md\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+main"
            .to_owned(),
        changed_files: vec!["release-notes.md".to_owned()],
        file_diffs: vec![ToolPreviewFile {
            path: "release-notes.md".to_owned(),
            diff: "--- current/release-notes.md\n+++ proposed/release-notes.md\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+main"
                .to_owned(),
        }],
    };
    store.append(&SessionLogEntry::Control(
        ControlEntry::ToolPreviewCaptured(ToolPreviewSnapshot::from_preview(
            "call-edit",
            "edit_file",
            &preview,
            Default::default(),
            Some("preview-hash".to_owned()),
        )),
    ))?;
    let recorder = sigil_kernel::MutationEventRecorder::new(store.clone());
    sigil_kernel::write_file_with_mutation(
        Some(&recorder),
        &workspace,
        "call-edit",
        "release-notes.md",
        &note,
        b"alpha\nmain\n",
    )?;
    assert!(app.restore_session_path_from_disk(
        session_path.clone(),
        "deepseek",
        "deepseek-v4-flash",
        "restored checkpoint preview fixture",
    ));
    app.set_terminal_size(DOC_SCREENSHOT_COLUMNS, DOC_SCREENSHOT_ROWS);
    let action = app
        .handle_key_event(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL))?
        .ok_or_else(|| anyhow::anyhow!("checkpoint preview action was not created"))?;
    let crate::app::AppAction::PreviewCheckpointRestore {
        request_id,
        request,
    } = action
    else {
        anyhow::bail!("unexpected checkpoint action");
    };
    let records = JsonlSessionStore::read_event_records(&session_path)?;
    let restore_preview = sigil_kernel::preview_controlled_checkpoint_restore(
        &recorder, &records, &workspace, &request,
    )?;
    app.handle_worker_message(WorkerMessage::CheckpointRestorePreviewed {
        request_id,
        preview: restore_preview,
    })?;
    Ok(app)
}

fn docs_compaction_preview_app() -> anyhow::Result<AppState> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let temp = tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("compaction-preview.jsonl"))?;
    store.append(&SessionLogEntry::User(ModelMessage::user(
        "Audit the documentation structure",
    )))?;
    store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
        Some("I reviewed the existing docs and site surfaces.".to_owned()),
        Vec::new(),
    )))?;
    store.append(&SessionLogEntry::User(ModelMessage::user(
        "Now synchronize the current main capabilities",
    )))?;
    let preview = store
        .v2_compaction_preview(1, None)?
        .ok_or_else(|| anyhow::anyhow!("compaction preview fixture was not foldable"))?;
    app.handle_worker_message(WorkerMessage::V2CompactionPreviewed {
        state: V2CompactionPreviewState::Review(Box::new(V2CompactionReview {
            request_id: 41,
            preview,
            admission: V2CompactionAdmission::Unavailable {
                reason: "apply is temporarily frozen while correctness fixes are in progress"
                    .to_owned(),
            },
        })),
    })?;
    app.set_terminal_size(DOC_SCREENSHOT_COLUMNS, DOC_SCREENSHOT_ROWS);
    Ok(app)
}

fn inject_write_file_approval(app: &mut AppState) -> anyhow::Result<()> {
    app.handle(RunEvent::ToolApprovalRequested {
        call: ToolCall {
            id: "call-1".to_owned(),
            name: "write_file".to_owned(),
            args_json: r#"{"path":"note.txt"}"#.to_owned(),
        },
        spec: ToolSpec {
            name: "write_file".to_owned(),
            description: "Write file".to_owned(),
            input_schema: json!({"type":"object"}),
            category: ToolCategory::File,
            access: ToolAccess::Write,
            network_effect: None,
            preview: ToolPreviewCapability::Required,
        },
        subjects: Vec::new(),
        network_effect: None,
        local_policy_decision: sigil_kernel::ApprovalMode::Ask,
        network_policy_decision: sigil_kernel::ApprovalMode::Allow,
        source_policy_decision: sigil_kernel::ApprovalMode::Allow,
        operation: sigil_kernel::ToolOperation::OverwriteFile,
        risk: sigil_kernel::PermissionRisk::Medium,
        subject_zones: Vec::new(),
        confirmation: None,
        snapshot_required: false,
        command_permission_matches: Vec::new(),
        preview: Some(ToolPreview {
            title: "Update note.txt".to_owned(),
            summary: "summary".to_owned(),
            body: [
                "--- note.txt",
                "+++ note.txt",
                "@@ -1 +1 @@",
                "-old",
                "+new",
            ]
            .join("\n"),
            changed_files: vec!["note.txt".to_owned()],
            file_diffs: vec![ToolPreviewFile {
                path: "note.txt".to_owned(),
                diff: [
                    "--- note.txt",
                    "+++ note.txt",
                    "@@ -1 +1 @@",
                    "-old",
                    "+new",
                ]
                .join("\n"),
            }],
        }),
    })
}

const DOC_SCREENSHOT_COLUMNS: u16 = 160;
const DOC_SCREENSHOT_ROWS: u16 = 36;
const DOC_CELL_WIDTH: u32 = 9;
const DOC_CELL_HEIGHT: u32 = 18;
const DOC_FONT_SIZE: u32 = 14;
const DOC_PADDING: u32 = 24;
const DOC_TITLE_BAR_HEIGHT: u32 = 42;

fn docs_session_app() -> anyhow::Result<AppState> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(DOC_SCREENSHOT_COLUMNS, DOC_SCREENSHOT_ROWS);
    app.composer.input = "Explain the repo layout and identify the main entrypoints.".to_owned();
    let _ = app.submit_input()?;
    app.handle(RunEvent::ReasoningDelta(
        "I will inspect the workspace metadata, crates, and user docs first.".to_owned(),
    ))?;

    let read_call = ToolCall {
        id: "call-read-cargo".to_owned(),
        name: "read_file".to_owned(),
        args_json: r#"{"path":"Cargo.toml"}"#.to_owned(),
    };
    app.handle(RunEvent::ToolCallStarted(read_call.clone()))?;
    app.handle(RunEvent::ToolCallCompleted(read_call.clone()))?;
    let mut read_meta = ToolResultMeta {
        returned_bytes: Some(4096),
        total_bytes: Some(4096),
        ..ToolResultMeta::default()
    };
    read_meta.details = json!({"path":"Cargo.toml"});
    app.handle(RunEvent::ToolResult(ToolResult::ok(
        read_call.id,
        read_call.name,
        "[workspace]\nmembers = [\"crates/sigil\", \"crates/sigil-tui\", ...]",
        read_meta,
    )))?;

    let grep_call = ToolCall {
        id: "call-grep-entrypoints".to_owned(),
        name: "grep".to_owned(),
        args_json: r#"{"pattern":"run_tui|Subcommand","path":"crates"}"#.to_owned(),
    };
    app.handle(RunEvent::ToolCallStarted(grep_call.clone()))?;
    app.handle(RunEvent::ToolCallCompleted(grep_call.clone()))?;
    let grep_meta = ToolResultMeta {
        returned_matches: Some(8),
        total_matches: Some(8),
        ..ToolResultMeta::default()
    };
    app.handle(RunEvent::ToolResult(ToolResult::ok(
        grep_call.id,
        grep_call.name,
        "crates/sigil/src/main.rs:92: run_tui\ncrates/sigil-tui/src/launcher.rs:53: run_tui",
        grep_meta,
    )))?;

    app.handle(RunEvent::TextDelta(
        "Sigil is TUI-first. Runtime wires providers and tools; kernel owns the agent contracts."
            .to_owned(),
    ))?;
    app.runtime.is_busy = false;
    Ok(app)
}

fn write_terminal_svg(
    path: &Path,
    title: &str,
    description: &str,
    app: &mut AppState,
) -> anyhow::Result<()> {
    let backend = TestBackend::new(DOC_SCREENSHOT_COLUMNS, DOC_SCREENSHOT_ROWS);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|frame| render(frame, app))?;
    fs::write(path, terminal_buffer_svg(&terminal, title, description))?;
    Ok(())
}

fn terminal_buffer_svg(terminal: &Terminal<TestBackend>, title: &str, description: &str) -> String {
    let buffer = terminal.backend().buffer();
    let columns = u32::from(buffer.area.width);
    let rows = u32::from(buffer.area.height);
    let terminal_width = columns * DOC_CELL_WIDTH;
    let terminal_height = rows * DOC_CELL_HEIGHT;
    let window_height = terminal_height + DOC_TITLE_BAR_HEIGHT;
    let width = terminal_width + DOC_PADDING * 2;
    let height = terminal_height + DOC_PADDING * 2 + DOC_TITLE_BAR_HEIGHT;
    let screen_x = DOC_PADDING;
    let screen_y = DOC_PADDING + DOC_TITLE_BAR_HEIGHT;
    let clip_id = title
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>();
    let clip_id = if clip_id.is_empty() {
        "terminalClip".to_owned()
    } else {
        format!("terminalClip{clip_id}")
    };
    let mut svg = String::new();

    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\" role=\"img\" aria-labelledby=\"title desc\">\n"
    ));
    svg.push_str(&format!(
        "  <title id=\"title\">{}</title>\n",
        svg_escape(title)
    ));
    svg.push_str(&format!(
        "  <desc id=\"desc\">{}</desc>\n",
        svg_escape(description)
    ));
    svg.push_str("  <rect width=\"100%\" height=\"100%\" rx=\"18\" fill=\"#101820\"/>\n");
    svg.push_str(&format!(
        "  <rect x=\"{screen_x}\" y=\"{DOC_PADDING}\" width=\"{terminal_width}\" height=\"{window_height}\" rx=\"14\" fill=\"#07080a\" stroke=\"#30414d\"/>\n"
    ));
    svg.push_str(&format!(
        "  <rect x=\"{screen_x}\" y=\"{DOC_PADDING}\" width=\"{terminal_width}\" height=\"{DOC_TITLE_BAR_HEIGHT}\" rx=\"14\" fill=\"#151a20\"/>\n"
    ));
    for (index, color) in ["#ff6b5f", "#f7c948", "#42d392"].iter().enumerate() {
        let cx = screen_x + 24 + index as u32 * 22;
        let cy = DOC_PADDING + DOC_TITLE_BAR_HEIGHT / 2;
        svg.push_str(&format!(
            "  <circle cx=\"{cx}\" cy=\"{cy}\" r=\"6\" fill=\"{color}\"/>\n"
        ));
    }
    svg.push_str(&format!(
        "  <text x=\"{}\" y=\"{}\" fill=\"#d9f6f3\" font-family=\"SFMono-Regular, ui-monospace, Menlo, Consolas, monospace\" font-size=\"14\">sigil</text>\n",
        screen_x + 92,
        DOC_PADDING + 27
    ));
    svg.push_str(&format!(
        "  <clipPath id=\"{clip_id}\"><rect x=\"{screen_x}\" y=\"{screen_y}\" width=\"{terminal_width}\" height=\"{terminal_height}\"/></clipPath>\n"
    ));
    svg.push_str(&format!("  <g clip-path=\"url(#{clip_id})\">\n"));
    svg.push_str(&format!(
        "    <rect x=\"{screen_x}\" y=\"{screen_y}\" width=\"{terminal_width}\" height=\"{terminal_height}\" fill=\"#07080a\"/>\n"
    ));

    for row in 0..buffer.area.height {
        let mut run_start = 0;
        while run_start < buffer.area.width {
            let run_bg = color_css(
                buffer
                    .cell((run_start, row))
                    .map(|cell| cell.bg)
                    .unwrap_or(Color::Reset),
                "#07080a",
            );
            let mut run_end = run_start + 1;
            while run_end < buffer.area.width
                && color_css(
                    buffer
                        .cell((run_end, row))
                        .map(|cell| cell.bg)
                        .unwrap_or(Color::Reset),
                    "#07080a",
                ) == run_bg
            {
                run_end += 1;
            }
            if run_bg != "#07080a" {
                let x = screen_x + u32::from(run_start) * DOC_CELL_WIDTH;
                let y = screen_y + u32::from(row) * DOC_CELL_HEIGHT;
                let width = u32::from(run_end - run_start) * DOC_CELL_WIDTH;
                svg.push_str(&format!(
                    "    <rect x=\"{x}\" y=\"{y}\" width=\"{width}\" height=\"{DOC_CELL_HEIGHT}\" fill=\"{run_bg}\"/>\n"
                ));
            }
            run_start = run_end;
        }
    }

    for row in 0..buffer.area.height {
        let mut column = 0;
        while column < buffer.area.width {
            let Some(cell) = buffer.cell((column, row)) else {
                column += 1;
                continue;
            };
            if cell.symbol().trim().is_empty() {
                column += 1;
                continue;
            }
            let run_start = column;
            let fg = color_css(cell.fg, "#ecf0f6");
            let weight = font_weight(cell.modifier);
            let mut text = String::new();
            while column < buffer.area.width {
                let Some(run_cell) = buffer.cell((column, row)) else {
                    break;
                };
                let run_fg = color_css(run_cell.fg, "#ecf0f6");
                if run_fg != fg || font_weight(run_cell.modifier) != weight {
                    break;
                }
                text.push_str(run_cell.symbol());
                column += 1;
            }
            let text = text.trim_end();
            if text.trim().is_empty() {
                continue;
            }
            let x = screen_x + u32::from(run_start) * DOC_CELL_WIDTH;
            let y = screen_y + u32::from(row) * DOC_CELL_HEIGHT + 14;
            svg.push_str(&format!(
                "    <text x=\"{x}\" y=\"{y}\" fill=\"{fg}\" font-family=\"SFMono-Regular, ui-monospace, Menlo, Consolas, monospace\" font-size=\"{DOC_FONT_SIZE}\" font-weight=\"{weight}\">{}</text>\n",
                svg_escape(text)
            ));
        }
    }

    svg.push_str("  </g>\n");
    svg.push_str("  <metadata>Generated from Sigil TUI renderer by render_docs_screenshot_assets.</metadata>\n");
    svg.push_str("</svg>\n");
    svg
}

fn color_css(color: Color, reset: &str) -> String {
    let (red, green, blue) = match color {
        Color::Reset => return reset.to_owned(),
        Color::Black => (0, 0, 0),
        Color::Red => (205, 49, 49),
        Color::Green => (13, 188, 121),
        Color::Yellow => (229, 229, 16),
        Color::Blue => (36, 114, 200),
        Color::Magenta => (188, 63, 188),
        Color::Cyan => (17, 168, 205),
        Color::Gray => (229, 229, 229),
        Color::DarkGray => (102, 102, 102),
        Color::LightRed => (241, 76, 76),
        Color::LightGreen => (35, 209, 139),
        Color::LightYellow => (245, 245, 67),
        Color::LightBlue => (59, 142, 234),
        Color::LightMagenta => (214, 112, 214),
        Color::LightCyan => (41, 184, 219),
        Color::White => (255, 255, 255),
        Color::Indexed(index) => indexed_color(index),
        Color::Rgb(red, green, blue) => (red, green, blue),
    };
    format!("#{red:02x}{green:02x}{blue:02x}")
}

fn font_weight(modifier: Modifier) -> &'static str {
    if modifier.contains(Modifier::BOLD) {
        "700"
    } else {
        "400"
    }
}

fn indexed_color(index: u8) -> (u8, u8, u8) {
    const ANSI_16: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (128, 0, 0),
        (0, 128, 0),
        (128, 128, 0),
        (0, 0, 128),
        (128, 0, 128),
        (0, 128, 128),
        (192, 192, 192),
        (128, 128, 128),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (0, 0, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];
    if index < 16 {
        return ANSI_16[index as usize];
    }
    if index < 232 {
        let offset = index - 16;
        let red = offset / 36;
        let green = (offset % 36) / 6;
        let blue = offset % 6;
        return (
            xterm_color_level(red),
            xterm_color_level(green),
            xterm_color_level(blue),
        );
    }
    let level = 8 + (index - 232) * 10;
    (level, level, level)
}

fn xterm_color_level(value: u8) -> u8 {
    if value == 0 { 0 } else { 55 + value * 40 }
}

fn svg_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
