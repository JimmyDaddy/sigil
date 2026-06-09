use std::{
    env, io,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use clap::Parser;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event as CrosstermEvent, KeyEventKind,
        KeyboardEnhancementFlags, MouseEventKind, PopKeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal, TerminalOptions, Viewport,
    backend::CrosstermBackend,
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use termquill_kernel::{RootConfig, preferred_config_path};
use termquill_tui::{
    app::AppState,
    runner::{self, WorkerMessage},
    ui,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const SEEDED_SCROLLBACK_MAX_LINES: usize = 120;
const BUSY_POLL_INTERVAL: Duration = Duration::from_millis(50);
const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(250);
const SPINNER_FRAME_MILLIS: u128 = 120;

#[derive(Parser)]
#[command(name = "termquill-tui")]
#[command(about = "TUI-first shell for Termquill")]
struct Cli {
    #[arg(long)]
    config: Option<PathBuf>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cwd = env::current_dir()?;
    let config_path = preferred_config_path(cli.config.as_deref(), &cwd)?;
    let mut worker = None;
    let mut app = match RootConfig::load(&config_path) {
        Ok(root_config) => {
            let mut app = AppState::from_root_config(&config_path, &root_config);
            app.restore_latest_session_from_disk(&root_config);
            worker = Some(spawn_worker(root_config.clone(), &app)?);
            app
        }
        Err(error) => AppState::from_setup(config_path.clone(), cwd, Some(error.to_string())),
    };

    enable_raw_mode()?;
    let inline_viewport_height = current_inline_viewport_height()?;
    let mut stdout = io::stdout();
    let keyboard_enhancement_enabled = enable_keyboard_enhancement(&mut stdout)?;
    execute!(stdout, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(inline_viewport_height),
        },
    )?;
    terminal.clear()?;

    let result = run_app(&mut terminal, &mut app, &mut worker);

    if keyboard_enhancement_enabled {
        execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags)?;
    }
    execute!(terminal.backend_mut(), DisableMouseCapture)?;
    disable_raw_mode()?;
    terminal.show_cursor()?;

    result
}

fn enable_keyboard_enhancement<W: io::Write>(writer: &mut W) -> io::Result<bool> {
    execute!(
        writer,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )?;
    Ok(true)
}

fn current_inline_viewport_height() -> Result<u16> {
    let (_, height) = crossterm::terminal::size()?;
    Ok(height.max(12))
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
) -> Result<()> {
    let mut scrollback = ScrollbackSyncState::default();
    let mut needs_render = true;
    let mut last_spinner_tick = live_spinner_tick();

    loop {
        let mut dirty = needs_render;
        if let Some(runtime) = worker.as_mut() {
            while let Ok(message) = runtime.worker_rx.try_recv() {
                app.handle_worker_message(message)?;
                dirty = true;
            }
        }
        dirty |= app.poll_background_tasks();

        let size = terminal.size()?;
        dirty |= app.set_terminal_size(size.width, size.height);

        let spinner_tick = live_spinner_tick();
        if app.is_busy && spinner_tick != last_spinner_tick {
            dirty = true;
        }

        if dirty {
            sync_terminal_scrollback(terminal, app, &mut scrollback)?;
            terminal.draw(|frame| ui::render(frame, app))?;
            last_spinner_tick = spinner_tick;
            needs_render = false;
        }

        if app.should_quit {
            if let Some(runtime) = worker.as_ref() {
                let _ = runtime.worker_tx.send(AppState::shutdown_command());
            }
            break;
        }

        let poll_interval = if app.is_busy {
            BUSY_POLL_INTERVAL
        } else {
            IDLE_POLL_INTERVAL
        };
        if event::poll(poll_interval)? {
            match event::read()? {
                CrosstermEvent::Resize(_, _) => {
                    terminal.autoresize()?;
                    needs_render = true;
                }
                CrosstermEvent::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        app.handle_mouse_scroll(true);
                        needs_render = true;
                    }
                    MouseEventKind::ScrollDown => {
                        app.handle_mouse_scroll(false);
                        needs_render = true;
                    }
                    _ => {}
                },
                CrosstermEvent::Key(key) if key.kind == KeyEventKind::Press => {
                    if let Some(action) = app.handle_key_event(key)? {
                        match action {
                            termquill_tui::app::AppAction::SetupCompleted {
                                config_path,
                                root_config,
                            } => {
                                *app = AppState::from_root_config(&config_path, &root_config);
                                app.restore_latest_session_from_disk(&root_config);
                                *worker = Some(spawn_worker(*root_config, app)?);
                            }
                            termquill_tui::app::AppAction::ConfigSaved { root_config } => {
                                if let Some(runtime) = worker.take() {
                                    let _ = runtime.worker_tx.send(AppState::shutdown_command());
                                }
                                *worker = Some(spawn_worker(*root_config, app)?);
                            }
                            termquill_tui::app::AppAction::RuntimeConfigUpdated { root_config } => {
                                if let Some(runtime) = worker.take() {
                                    let _ = runtime.worker_tx.send(AppState::shutdown_command());
                                }
                                *worker = Some(spawn_worker(*root_config, app)?);
                            }
                            action => {
                                if let Some(runtime) = worker.as_ref() {
                                    runtime.worker_tx.send(app.into_worker_command(action))?;
                                }
                            }
                        }
                    }
                    needs_render = true;
                }
                _ => {}
            }
        }
    }

    Ok(())
}

fn live_spinner_tick() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() / SPINNER_FRAME_MILLIS)
        .unwrap_or(0)
}

#[derive(Default)]
struct ScrollbackSyncState {
    session_id: Option<String>,
    revision: u64,
    line_count: usize,
    sequence_hash: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrollbackSyncPlan {
    Seed { insert_separator: bool },
    Append { from_index: usize },
    Noop,
}

fn sync_terminal_scrollback(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &AppState,
    sync_state: &mut ScrollbackSyncState,
) -> Result<()> {
    if !should_sync_terminal_scrollback(app) {
        return Ok(());
    }
    if sync_state.session_id.as_deref() == Some(app.session_id.as_str())
        && sync_state.revision == app.timeline_revision()
    {
        return Ok(());
    }

    let next_line_count = app.scrollback_line_count();
    let shared_len = sync_state.line_count.min(next_line_count);
    let shared_hash = app.scrollback_prefix_hash(shared_len);

    match plan_scrollback_sync(
        sync_state,
        app.session_id.as_str(),
        next_line_count,
        shared_hash,
    ) {
        ScrollbackSyncPlan::Seed { insert_separator } => {
            if insert_separator {
                insert_scrollback_lines(
                    terminal,
                    vec![scrollback_separator(app), Line::raw(String::new())],
                )?;
            }
            let seed_from_index = next_line_count.saturating_sub(SEEDED_SCROLLBACK_MAX_LINES);
            insert_scrollback_lines(terminal, app.scrollback_lines_from(seed_from_index))?;
        }
        ScrollbackSyncPlan::Append { from_index } => {
            let appended = app.scrollback_lines_from(from_index);
            insert_scrollback_lines(terminal, appended)?;
        }
        ScrollbackSyncPlan::Noop => {}
    }

    sync_state.session_id = Some(app.session_id.clone());
    sync_state.revision = app.timeline_revision();
    sync_state.line_count = next_line_count;
    sync_state.sequence_hash = app.scrollback_prefix_hash(next_line_count);
    Ok(())
}

fn should_sync_terminal_scrollback(app: &AppState) -> bool {
    !app.is_busy && !app.is_setup_mode() && !app.is_config_mode()
}

fn plan_scrollback_sync(
    sync_state: &ScrollbackSyncState,
    session_id: &str,
    next_line_count: usize,
    shared_hash: u64,
) -> ScrollbackSyncPlan {
    let session_changed = sync_state.session_id.as_deref() != Some(session_id);
    if session_changed {
        return ScrollbackSyncPlan::Seed {
            insert_separator: sync_state.session_id.is_some() && sync_state.line_count > 0,
        };
    }
    if next_line_count > sync_state.line_count && shared_hash == sync_state.sequence_hash {
        return ScrollbackSyncPlan::Append {
            from_index: sync_state.line_count,
        };
    }
    ScrollbackSyncPlan::Noop
}

fn insert_scrollback_lines(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    lines: Vec<Line<'static>>,
) -> Result<()> {
    if lines.is_empty() {
        return Ok(());
    }

    let width = terminal.size()?.width.max(1) as usize;
    let rows = lines
        .iter()
        .flat_map(|line| scrollback_wrapped_rows(line, width))
        .collect::<Vec<_>>();
    let height = rows.len().max(1) as u16;
    terminal.insert_before(height, |buf| {
        render_scrollback_rows(buf, &rows);
    })?;
    Ok(())
}

fn scrollback_plain_line(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

fn scrollback_wrapped_rows(line: &Line<'_>, width: usize) -> Vec<(String, Style)> {
    let plain = scrollback_plain_line(line);
    let style = scrollback_row_style(line);
    wrap_scrollback_text(&plain, width)
        .into_iter()
        .map(|row| (row, style))
        .collect()
}

fn scrollback_row_style(line: &Line<'_>) -> Style {
    line.spans
        .iter()
        .find(|span| !span.content.trim().is_empty())
        .map(|span| {
            let mut style = Style::default();
            if let Some(color) = span.style.fg {
                style = style.fg(color);
            }
            if span.style.add_modifier.contains(Modifier::BOLD) {
                style = style.add_modifier(Modifier::BOLD);
            }
            style
        })
        .unwrap_or_default()
}

fn wrap_scrollback_text(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() || width == 0 {
        return vec![text.to_owned()];
    }

    let mut rows = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;

    for grapheme in text.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme).max(1);
        if !current.is_empty() && current_width + grapheme_width > width {
            rows.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push_str(grapheme);
        current_width += grapheme_width;
    }

    if current.is_empty() {
        rows.push(String::new());
    } else {
        rows.push(current);
    }

    rows
}

fn render_scrollback_rows(buf: &mut Buffer, rows: &[(String, Style)]) {
    let area = Rect {
        x: 0,
        y: 0,
        width: buf.area.width,
        height: buf.area.height,
    };
    for (y, (row, style)) in rows.iter().enumerate().take(area.height as usize) {
        let cell = &mut buf[(0, y as u16)];
        cell.set_symbol(row);
        cell.set_style(*style);
    }
}

fn scrollback_separator(app: &AppState) -> Line<'static> {
    Line::from(vec![
        Span::styled("---- session ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            app.session_id.chars().take(8).collect::<String>(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {} / {} ----", app.provider_name, app.model_name),
            Style::default().fg(Color::DarkGray),
        ),
    ])
}

struct WorkerRuntime {
    worker_tx: std::sync::mpsc::Sender<runner::WorkerCommand>,
    worker_rx: std::sync::mpsc::Receiver<WorkerMessage>,
}

fn spawn_worker(root_config: RootConfig, app: &AppState) -> Result<WorkerRuntime> {
    let (worker_tx, worker_rx) = runner::spawn_agent_worker(
        root_config,
        app.session_log_path.clone(),
        app.workspace_root.clone(),
        termquill_kernel::InteractionMode::Interactive,
    )?;
    Ok(WorkerRuntime {
        worker_tx,
        worker_rx,
    })
}

#[cfg(test)]
#[path = "tests/main_tests.rs"]
mod tests;
