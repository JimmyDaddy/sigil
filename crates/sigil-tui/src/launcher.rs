use std::{path::PathBuf, time::Duration};

#[cfg(not(test))]
use std::{
    env, io,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
#[cfg(not(test))]
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event as CrosstermEvent, KeyEventKind,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};
#[cfg(not(test))]
use ratatui::{Terminal, TerminalOptions, Viewport, backend::CrosstermBackend};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use sigil_kernel::RootConfig;
#[cfg(not(test))]
use sigil_kernel::preferred_config_path;

#[cfg(not(test))]
use crate::ui;
use crate::{
    app::{AppAction, AppState},
    mouse::AppMouseOutcome,
    runner::{self, WorkerCommand, WorkerMessage},
    ui::LayoutSnapshot,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const BUSY_POLL_INTERVAL: Duration = Duration::from_millis(50);
const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(250);
const SCROLLBACK_SEED_POLL_INTERVAL: Duration = Duration::from_millis(16);
// Seed restored scrollback in one pass so startup does not visibly redraw chunk by chunk.
const SCROLLBACK_SEED_CHUNK_LINES: usize = usize::MAX;
#[cfg(not(test))]
const MAX_SCROLLBACK_INSERT_ROWS: usize = u16::MAX as usize;
#[cfg(not(test))]
const SPINNER_FRAME_MILLIS: u128 = 120;

#[cfg(not(test))]
pub fn run_tui(config: Option<PathBuf>) -> Result<()> {
    let cwd = env::current_dir()?;
    let config_path = preferred_config_path(config.as_deref(), &cwd)?;
    let (mut app, mut worker) = build_initial_app(
        cwd,
        config_path.clone(),
        RootConfig::load(&config_path),
        spawn_worker,
    )?;

    enable_raw_mode()?;
    let inline_viewport_height = current_inline_viewport_height()?;
    let mut stdout = io::stdout();
    let keyboard_enhancement_enabled = enable_keyboard_enhancement(&mut stdout)?;
    let mut mouse_capture_active = app.terminal_mouse_capture_enabled();
    if mouse_capture_active {
        execute!(stdout, EnableMouseCapture)?;
    }
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(inline_viewport_height),
        },
    )?;
    let result = run_app(
        &mut terminal,
        &mut app,
        &mut worker,
        &mut mouse_capture_active,
    );

    if keyboard_enhancement_enabled {
        execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags)?;
    }
    if mouse_capture_active {
        execute!(terminal.backend_mut(), DisableMouseCapture)?;
    }
    disable_raw_mode()?;
    terminal.show_cursor()?;

    result
}

#[cfg(not(test))]
fn enable_keyboard_enhancement<W: io::Write>(writer: &mut W) -> io::Result<bool> {
    execute!(
        writer,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )?;
    Ok(true)
}

#[cfg(not(test))]
fn current_inline_viewport_height() -> Result<u16> {
    let (_, height) = crossterm::terminal::size()?;
    Ok(height.max(12))
}

#[cfg(not(test))]
fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    mouse_capture_active: &mut bool,
) -> Result<()> {
    let mut scrollback = ScrollbackSyncState::default();
    let mut needs_render = true;
    let mut last_spinner_tick = live_spinner_tick();
    let mut latest_frame_area = Rect::default();

    loop {
        let mut dirty = needs_render;
        dirty |= drain_worker_messages(app, worker)?;
        dirty |= app.poll_background_tasks();
        dirty |= flush_pending_worker_commands(app, worker)?;
        if let Some(enable) =
            next_mouse_capture_action(*mouse_capture_active, app.terminal_mouse_capture_enabled())
        {
            if enable {
                execute!(terminal.backend_mut(), EnableMouseCapture)?;
            } else {
                execute!(terminal.backend_mut(), DisableMouseCapture)?;
            }
            *mouse_capture_active = enable;
            dirty = true;
        }

        let size = terminal.size()?;
        dirty |= app.set_terminal_size(size.width, size.height);
        dirty |= scrollback.has_pending_seed() && should_sync_terminal_scrollback(app);

        let spinner_tick = live_spinner_tick();
        if app.is_busy && spinner_tick != last_spinner_tick {
            dirty = true;
        }

        if dirty {
            sync_terminal_scrollback(terminal, app, &mut scrollback)?;
            terminal.draw(|frame| {
                latest_frame_area = frame.area();
                ui::render(frame, app);
            })?;
            last_spinner_tick = spinner_tick;
            needs_render = false;
        }

        if app.should_quit {
            if let Some(runtime) = worker.as_ref() {
                let _ = runtime.worker_tx.send(AppState::shutdown_command());
            }
            break;
        }

        if event::poll(poll_interval(app, &scrollback))? {
            match event::read()? {
                CrosstermEvent::Resize(_, _) => {
                    terminal.autoresize()?;
                    needs_render = true;
                }
                CrosstermEvent::Mouse(mouse) => {
                    let layout = mouse_layout_snapshot(latest_frame_area, size.into(), app);
                    let outcome = app.handle_mouse_event(mouse.into(), &layout)?;
                    needs_render |= apply_mouse_outcome(app, worker, outcome, spawn_worker)?;
                }
                CrosstermEvent::Key(key) if key.kind == KeyEventKind::Press => {
                    let action = app.handle_key_event(key)?;
                    needs_render |= apply_key_action(app, worker, action, spawn_worker)?;
                }
                _ => {}
            }
        }
    }

    Ok(())
}

fn mouse_layout_snapshot(frame_area: Rect, terminal_size: Rect, app: &AppState) -> LayoutSnapshot {
    let screen = if frame_area.width == 0 || frame_area.height == 0 {
        Rect::new(0, 0, terminal_size.width, terminal_size.height)
    } else {
        frame_area
    };
    LayoutSnapshot::from_app(screen, app)
}

fn build_initial_app<F>(
    cwd: PathBuf,
    config_path: PathBuf,
    load_result: Result<RootConfig>,
    mut spawn_worker_fn: F,
) -> Result<(AppState, Option<WorkerRuntime>)>
where
    F: FnMut(RootConfig, &AppState) -> Result<WorkerRuntime>,
{
    let mut worker = None;
    let app = match load_result {
        Ok(root_config) => {
            let mut app = AppState::from_root_config(&config_path, &root_config);
            app.restore_latest_session_from_disk(&root_config);
            worker = Some(spawn_worker_fn(root_config, &app)?);
            flush_pending_worker_commands(&mut app, &mut worker)?;
            app
        }
        Err(error) => AppState::from_setup(config_path.clone(), cwd, Some(error.to_string())),
    };
    Ok((app, worker))
}

fn process_app_action_with_spawner<F>(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    action: AppAction,
    mut spawn_worker_fn: F,
) -> Result<()>
where
    F: FnMut(RootConfig, &AppState) -> Result<WorkerRuntime>,
{
    match action {
        AppAction::SetupCompleted {
            config_path,
            root_config,
        } => {
            *app = AppState::from_root_config(&config_path, &root_config);
            app.restore_latest_session_from_disk(&root_config);
            *worker = Some(spawn_worker_fn(*root_config, app)?);
        }
        AppAction::ConfigSaved { root_config }
        | AppAction::RuntimeConfigUpdated { root_config } => {
            if let Some(runtime) = worker.take() {
                let _ = runtime.worker_tx.send(AppState::shutdown_command());
            }
            *worker = Some(spawn_worker_fn(*root_config, app)?);
        }
        AppAction::CopyToClipboard { text } => {
            if app.terminal_osc52_clipboard_enabled() {
                copy_text_to_terminal_clipboard(&text)?;
                app.record_clipboard_copy_success(&text);
            } else {
                app.record_clipboard_copy_unavailable("OSC52 disabled");
            }
        }
        action => {
            let command = app.into_worker_command(action);
            send_worker_command_with_restart(app, worker, command, &mut spawn_worker_fn)?;
        }
    }
    flush_pending_worker_commands(app, worker)?;
    Ok(())
}

#[cfg(not(test))]
fn copy_text_to_terminal_clipboard(text: &str) -> Result<()> {
    use std::io::Write as _;

    let sequence = osc52_clipboard_sequence(text);
    let mut stdout = io::stdout();
    stdout.write_all(sequence.as_bytes())?;
    stdout.flush()?;
    Ok(())
}

#[cfg(test)]
fn copy_text_to_terminal_clipboard(_text: &str) -> Result<()> {
    Ok(())
}

fn osc52_clipboard_sequence(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64_encode(text.as_bytes()))
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let first = chunk[0];
        let second = *chunk.get(1).unwrap_or(&0);
        let third = *chunk.get(2).unwrap_or(&0);
        output.push(TABLE[(first >> 2) as usize] as char);
        output.push(TABLE[(((first & 0b0000_0011) << 4) | (second >> 4)) as usize] as char);
        if chunk.len() > 1 {
            output.push(TABLE[(((second & 0b0000_1111) << 2) | (third >> 6)) as usize] as char);
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(TABLE[(third & 0b0011_1111) as usize] as char);
        } else {
            output.push('=');
        }
    }
    output
}

#[cfg(test)]
fn process_app_action(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    action: AppAction,
) -> Result<()> {
    process_app_action_with_spawner(app, worker, action, |_root_config, _app| {
        Err(anyhow::anyhow!(
            "test wrapper should not spawn a real worker"
        ))
    })
}

#[cfg(not(test))]
fn live_spinner_tick() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() / SPINNER_FRAME_MILLIS)
        .unwrap_or(0)
}

fn drain_worker_messages(app: &mut AppState, worker: &mut Option<WorkerRuntime>) -> Result<bool> {
    let Some(runtime) = worker.as_mut() else {
        return Ok(false);
    };
    let mut dirty = false;
    app.begin_timeline_render_batch();
    while let Ok(message) = runtime.worker_rx.try_recv() {
        app.handle_worker_message(message)?;
        dirty = true;
    }
    Ok(dirty | app.flush_timeline_render_batch())
}

fn flush_pending_worker_commands(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
) -> Result<bool> {
    if !app.has_pending_worker_commands() {
        return Ok(false);
    }
    if worker.is_none() {
        return Ok(false);
    }
    let commands = app.drain_pending_worker_commands();
    let dirty = !commands.is_empty();
    for command in commands {
        if send_worker_command(app, worker, command)? {
            continue;
        }
        break;
    }
    Ok(dirty)
}

fn send_worker_command(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    command: WorkerCommand,
) -> Result<bool> {
    let Some(runtime) = worker.as_ref() else {
        return Ok(false);
    };
    match runtime.worker_tx.send(command) {
        Ok(()) => Ok(true),
        Err(_) => {
            *worker = None;
            report_worker_unavailable(app, "agent worker stopped before accepting command")?;
            Ok(false)
        }
    }
}

fn send_worker_command_with_restart<F>(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    command: WorkerCommand,
    spawn_worker_fn: &mut F,
) -> Result<()>
where
    F: FnMut(RootConfig, &AppState) -> Result<WorkerRuntime>,
{
    let command = if let Some(runtime) = worker.as_ref() {
        match runtime.worker_tx.send(command) {
            Ok(()) => return Ok(()),
            Err(error) => {
                *worker = None;
                error.0
            }
        }
    } else {
        command
    };

    let Some(root_config) = app.root_config_snapshot().cloned() else {
        report_worker_unavailable(app, "agent worker stopped; runtime config unavailable")?;
        return Ok(());
    };

    match spawn_worker_fn(root_config, app) {
        Ok(runtime) => {
            *worker = Some(runtime);
        }
        Err(error) => {
            report_worker_unavailable(app, &format!("failed to restart agent worker: {error:#}"))?;
            return Ok(());
        }
    }

    if let Some(runtime) = worker.as_ref()
        && runtime.worker_tx.send(command).is_ok()
    {
        return Ok(());
    }
    *worker = None;
    report_worker_unavailable(app, "agent worker stopped before accepting command")
}

fn report_worker_unavailable(app: &mut AppState, message: &str) -> Result<()> {
    app.handle_worker_message(WorkerMessage::RunFailed(message.to_owned()))
}

fn apply_mouse_outcome<F>(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    outcome: AppMouseOutcome,
    mut spawn_worker_fn: F,
) -> Result<bool>
where
    F: FnMut(RootConfig, &AppState) -> Result<WorkerRuntime>,
{
    match outcome {
        AppMouseOutcome::Noop => Ok(false),
        AppMouseOutcome::Redraw => Ok(true),
        AppMouseOutcome::Action(action) => {
            process_app_action_with_spawner(app, worker, action, &mut spawn_worker_fn)?;
            Ok(true)
        }
    }
}

fn apply_key_action<F>(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    action: Option<AppAction>,
    mut spawn_worker_fn: F,
) -> Result<bool>
where
    F: FnMut(RootConfig, &AppState) -> Result<WorkerRuntime>,
{
    if let Some(action) = action {
        process_app_action_with_spawner(app, worker, action, &mut spawn_worker_fn)?;
    }
    Ok(true)
}

fn next_mouse_capture_action(active: bool, desired: bool) -> Option<bool> {
    if active == desired {
        return None;
    }
    Some(desired)
}

#[derive(Debug, Clone, Default)]
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

#[derive(Debug, Clone)]
struct PreparedScrollbackSync {
    line_batches: Vec<Vec<Line<'static>>>,
    next_state: ScrollbackSyncState,
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

#[cfg(not(test))]
fn sync_terminal_scrollback(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &AppState,
    sync_state: &mut ScrollbackSyncState,
) -> Result<()> {
    let Some(prepared) = prepare_scrollback_sync(app, sync_state) else {
        return Ok(());
    };
    for batch in prepared.line_batches {
        insert_scrollback_lines(terminal, batch)?;
    }
    *sync_state = prepared.next_state;
    Ok(())
}

fn should_sync_terminal_scrollback(app: &AppState) -> bool {
    !app.is_busy && !app.is_setup_mode() && !app.is_config_mode()
}

fn poll_interval(app: &AppState, scrollback: &ScrollbackSyncState) -> Duration {
    if app.is_busy {
        BUSY_POLL_INTERVAL
    } else if scrollback.has_pending_seed() && should_sync_terminal_scrollback(app) {
        SCROLLBACK_SEED_POLL_INTERVAL
    } else {
        IDLE_POLL_INTERVAL
    }
}

#[cfg(test)]
fn next_poll_interval(app: &AppState, scrollback: &ScrollbackSyncState) -> Duration {
    poll_interval(app, scrollback)
}

fn prepare_scrollback_sync(
    app: &AppState,
    sync_state: &ScrollbackSyncState,
) -> Option<PreparedScrollbackSync> {
    prepare_scrollback_sync_with_chunk_size(app, sync_state, SCROLLBACK_SEED_CHUNK_LINES)
}

fn prepare_scrollback_sync_with_chunk_size(
    app: &AppState,
    sync_state: &ScrollbackSyncState,
    chunk_size: usize,
) -> Option<PreparedScrollbackSync> {
    if !should_sync_terminal_scrollback(app) {
        return None;
    }
    if sync_state.session_id.as_deref() == Some(app.session_id.as_str())
        && sync_state.revision == app.timeline_revision()
        && sync_state.pending_seed.is_none()
    {
        return None;
    }

    let next_line_count = app.scrollback_line_count();
    let shared_len = sync_state.line_count.min(next_line_count);
    let shared_hash = app.scrollback_prefix_hash(shared_len);
    let plan = plan_scrollback_sync_with_chunk_size(
        sync_state,
        app.session_id.as_str(),
        next_line_count,
        shared_hash,
        chunk_size,
    );
    let mut line_batches = Vec::new();
    let mut next_state = ScrollbackSyncState {
        session_id: Some(app.session_id.clone()),
        revision: app.timeline_revision(),
        line_count: next_line_count,
        sequence_hash: app.scrollback_prefix_hash(next_line_count),
        pending_seed: None,
    };

    match plan {
        ScrollbackSyncPlan::Seed {
            insert_separator,
            from_index,
            to_index,
            total_line_count,
        } => {
            if insert_separator {
                line_batches.push(vec![scrollback_separator(app), Line::raw(String::new())]);
            }
            let seeded = app.scrollback_lines_range(from_index, to_index);
            if !seeded.is_empty() {
                line_batches.push(seeded);
            }
            next_state.pending_seed =
                (to_index < total_line_count).then(|| ScrollbackSeedProgress {
                    session_id: app.session_id.clone(),
                    next_line_index: to_index,
                });
            next_state.line_count = to_index;
            next_state.sequence_hash = app.scrollback_prefix_hash(to_index);
        }
        ScrollbackSyncPlan::Append { from_index } => {
            let appended = app.scrollback_lines_from(from_index);
            if !appended.is_empty() {
                line_batches.push(appended);
            }
        }
        ScrollbackSyncPlan::Noop => {}
    }

    Some(PreparedScrollbackSync {
        line_batches,
        next_state,
    })
}

#[cfg(test)]
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

#[cfg(not(test))]
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
    for chunk in rows.chunks(MAX_SCROLLBACK_INSERT_ROWS) {
        let height = chunk.len().max(1) as u16;
        terminal.insert_before(height, |buf| {
            render_scrollback_rows(buf, chunk);
        })?;
    }
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

    rows.push(current);

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

#[cfg(not(test))]
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

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/main_tests.rs"]
mod tests;
