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
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
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
use sigil_kernel::{RootConfig, preferred_config_path};
use sigil_tui::{
    app::{AppAction, AppState},
    mouse::AppMouseOutcome,
    runner::{self, WorkerMessage},
    ui::{self, LayoutSnapshot},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const BUSY_POLL_INTERVAL: Duration = Duration::from_millis(50);
const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(250);
const SCROLLBACK_SEED_POLL_INTERVAL: Duration = Duration::from_millis(16);
const SCROLLBACK_SEED_CHUNK_LINES: usize = 160;
const SPINNER_FRAME_MILLIS: u128 = 120;

#[derive(Parser)]
#[command(name = "sigil-tui")]
#[command(about = "TUI-first shell for Sigil")]
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
            app.begin_timeline_render_batch();
            while let Ok(message) = runtime.worker_rx.try_recv() {
                app.handle_worker_message(message)?;
                dirty = true;
            }
            dirty |= app.flush_timeline_render_batch();
        }
        dirty |= app.poll_background_tasks();

        let size = terminal.size()?;
        dirty |= app.set_terminal_size(size.width, size.height);
        dirty |= scrollback.has_pending_seed() && should_sync_terminal_scrollback(app);

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

        let poll_interval = next_poll_interval(app, &scrollback);
        if event::poll(poll_interval)? {
            match event::read()? {
                CrosstermEvent::Resize(_, _) => {
                    terminal.autoresize()?;
                    needs_render = true;
                }
                CrosstermEvent::Mouse(mouse) => {
                    let layout =
                        LayoutSnapshot::from_app(Rect::new(0, 0, size.width, size.height), app);
                    match app.handle_mouse_event(mouse.into(), &layout)? {
                        AppMouseOutcome::Noop => {}
                        AppMouseOutcome::Redraw => {
                            needs_render = true;
                        }
                        AppMouseOutcome::Action(action) => {
                            process_app_action(app, worker, action)?;
                            needs_render = true;
                        }
                    }
                }
                CrosstermEvent::Key(key) if key.kind == KeyEventKind::Press => {
                    if let Some(action) = app.handle_key_event(key)? {
                        process_app_action(app, worker, action)?;
                    }
                    needs_render = true;
                }
                _ => {}
            }
        }
    }

    Ok(())
}

fn process_app_action(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    action: AppAction,
) -> Result<()> {
    match action {
        AppAction::SetupCompleted {
            config_path,
            root_config,
        } => {
            *app = AppState::from_root_config(&config_path, &root_config);
            app.restore_latest_session_from_disk(&root_config);
            *worker = Some(spawn_worker(*root_config, app)?);
        }
        AppAction::ConfigSaved { root_config }
        | AppAction::RuntimeConfigUpdated { root_config } => {
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
    pending_seed: Option<ScrollbackSeedProgress>,
}

impl ScrollbackSyncState {
    fn has_pending_seed(&self) -> bool {
        self.pending_seed.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScrollbackSeedProgress {
    session_id: String,
    next_line_index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrollbackSyncPlan {
    Seed {
        insert_separator: bool,
        from_index: usize,
        to_index: usize,
        total_line_count: usize,
    },
    Append {
        from_index: usize,
    },
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
        && sync_state.pending_seed.is_none()
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
        ScrollbackSyncPlan::Seed {
            insert_separator,
            from_index,
            to_index,
            total_line_count,
        } => {
            if insert_separator {
                insert_scrollback_lines(
                    terminal,
                    vec![scrollback_separator(app), Line::raw(String::new())],
                )?;
            }
            insert_scrollback_lines(terminal, app.scrollback_lines_range(from_index, to_index))?;
            sync_state.pending_seed = if to_index < total_line_count {
                Some(ScrollbackSeedProgress {
                    session_id: app.session_id.clone(),
                    next_line_index: to_index,
                })
            } else {
                None
            };
            sync_state.line_count = to_index;
            sync_state.sequence_hash = app.scrollback_prefix_hash(to_index);
        }
        ScrollbackSyncPlan::Append { from_index } => {
            let appended = app.scrollback_lines_from(from_index);
            insert_scrollback_lines(terminal, appended)?;
            sync_state.pending_seed = None;
            sync_state.line_count = next_line_count;
            sync_state.sequence_hash = app.scrollback_prefix_hash(next_line_count);
        }
        ScrollbackSyncPlan::Noop => {
            sync_state.pending_seed = None;
            sync_state.line_count = next_line_count;
            sync_state.sequence_hash = app.scrollback_prefix_hash(next_line_count);
        }
    }

    sync_state.session_id = Some(app.session_id.clone());
    sync_state.revision = app.timeline_revision();
    Ok(())
}

fn should_sync_terminal_scrollback(app: &AppState) -> bool {
    !app.is_busy && !app.is_setup_mode() && !app.is_config_mode()
}

fn next_poll_interval(app: &AppState, scrollback: &ScrollbackSyncState) -> Duration {
    if app.is_busy {
        BUSY_POLL_INTERVAL
    } else if scrollback.has_pending_seed() && should_sync_terminal_scrollback(app) {
        SCROLLBACK_SEED_POLL_INTERVAL
    } else {
        IDLE_POLL_INTERVAL
    }
}

fn plan_scrollback_sync(
    sync_state: &ScrollbackSyncState,
    session_id: &str,
    next_line_count: usize,
    shared_hash: u64,
) -> ScrollbackSyncPlan {
    plan_scrollback_sync_with_chunk_size(
        sync_state,
        session_id,
        next_line_count,
        shared_hash,
        SCROLLBACK_SEED_CHUNK_LINES,
    )
}

fn plan_scrollback_sync_with_chunk_size(
    sync_state: &ScrollbackSyncState,
    session_id: &str,
    next_line_count: usize,
    shared_hash: u64,
    chunk_size: usize,
) -> ScrollbackSyncPlan {
    let chunk_size = chunk_size.max(1);
    if let Some(pending_seed) = &sync_state.pending_seed {
        let pending_seed_matches = sync_state.session_id.as_deref() == Some(session_id)
            && pending_seed.session_id == session_id
            && pending_seed.next_line_index == sync_state.line_count
            && pending_seed.next_line_index <= next_line_count;
        if pending_seed_matches && pending_seed.next_line_index < next_line_count {
            let to_index = pending_seed
                .next_line_index
                .saturating_add(chunk_size)
                .min(next_line_count);
            return ScrollbackSyncPlan::Seed {
                insert_separator: false,
                from_index: pending_seed.next_line_index,
                to_index,
                total_line_count: next_line_count,
            };
        }
    }

    let session_changed = sync_state.session_id.as_deref() != Some(session_id);
    if session_changed {
        let to_index = chunk_size.min(next_line_count);
        return ScrollbackSyncPlan::Seed {
            insert_separator: sync_state.session_id.is_some() && sync_state.line_count > 0,
            from_index: 0,
            to_index,
            total_line_count: next_line_count,
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
        sigil_kernel::InteractionMode::Interactive,
    )?;
    Ok(WorkerRuntime {
        worker_tx,
        worker_rx,
    })
}

#[cfg(test)]
#[path = "tests/main_tests.rs"]
mod tests;
