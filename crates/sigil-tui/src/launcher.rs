use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

#[cfg(not(test))]
use std::{
    env, io,
    panic::{self, AssertUnwindSafe},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
#[cfg(not(test))]
use crossterm::{
    cursor::MoveTo,
    cursor::Show,
    event::{
        self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture, Event as CrosstermEvent, EventStream, KeyCode,
        KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{
        Clear, ClearType, disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement,
    },
};
#[cfg(not(test))]
use futures::StreamExt;
#[cfg(not(test))]
use ratatui::{Terminal, TerminalOptions, Viewport, backend::CrosstermBackend};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
};
#[cfg(not(test))]
use sigil_kernel::TerminalKeyboardEnhancement;
#[cfg(not(test))]
use sigil_kernel::preferred_config_path;
use sigil_kernel::{JsonlSessionStore, RootConfig};
#[cfg(not(test))]
use sigil_runtime::support::SupportBuildInfo;
#[cfg(not(test))]
use sigil_updater::BuildMetadata;

#[cfg(not(test))]
use crate::terminal_backend::CachedCursorBackend;
#[cfg(not(test))]
use crate::ui;
use crate::{
    app::{AppAction, AppState},
    attention::AttentionController,
    clipboard::{self, ClipboardCopyOutcome},
    mouse::AppMouseOutcome,
    runner::{self, WorkerCommand, WorkerMessage},
    ui::{LayoutSnapshot, theme},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const BACKGROUND_TASK_WAKE_INTERVAL: Duration = Duration::from_millis(250);
const SCROLLBACK_SEED_POLL_INTERVAL: Duration = Duration::from_millis(16);
#[cfg(not(test))]
const EVENT_BATCH_LIMIT: usize = 64;
// Keep restored scrollback seeding small enough that startup reaches the first
// interactive frame quickly even when the previous session has a long timeline.
const SCROLLBACK_SEED_CHUNK_LINES: usize = 256;
#[cfg(not(test))]
const MAX_SCROLLBACK_INSERT_ROWS: usize = u16::MAX as usize;
const SPINNER_FRAME_MILLIS: u128 = 120;

#[cfg(not(test))]
type TuiTerminal = Terminal<CachedCursorBackend<CrosstermBackend<io::Stdout>>>;

#[cfg(not(test))]
pub fn run_tui(config: Option<PathBuf>) -> Result<()> {
    run_tui_with_build_info(config, SupportBuildInfo::unknown())
}

#[cfg(not(test))]
pub fn run_tui_with_build_info(
    config: Option<PathBuf>,
    build_info: SupportBuildInfo,
) -> Result<()> {
    let update_info = BuildMetadata::source(
        build_info.version.clone(),
        build_info.target.clone(),
        build_info.profile.clone(),
    );
    run_tui_with_build_context(config, build_info, update_info)
}

#[cfg(not(test))]
pub fn run_tui_with_build_context(
    config: Option<PathBuf>,
    build_info: SupportBuildInfo,
    update_info: BuildMetadata,
) -> Result<()> {
    run_tui_with_initial_session(config, InitialSessionTarget::Fresh, build_info, update_info)
}

#[cfg(not(test))]
pub fn run_tui_resume(config: Option<PathBuf>, session_selector: Option<String>) -> Result<()> {
    run_tui_resume_with_build_info(config, session_selector, SupportBuildInfo::unknown())
}

#[cfg(not(test))]
pub fn run_tui_resume_with_build_info(
    config: Option<PathBuf>,
    session_selector: Option<String>,
    build_info: SupportBuildInfo,
) -> Result<()> {
    let update_info = BuildMetadata::source(
        build_info.version.clone(),
        build_info.target.clone(),
        build_info.profile.clone(),
    );
    run_tui_resume_with_build_context(config, session_selector, build_info, update_info)
}

#[cfg(not(test))]
pub fn run_tui_resume_with_build_context(
    config: Option<PathBuf>,
    session_selector: Option<String>,
    build_info: SupportBuildInfo,
    update_info: BuildMetadata,
) -> Result<()> {
    let target = session_selector
        .as_deref()
        .map(InitialSessionTarget::Selector)
        .unwrap_or(InitialSessionTarget::Latest);
    run_tui_with_initial_session(config, target, build_info, update_info)
}

#[cfg(not(test))]
fn run_tui_with_initial_session(
    config: Option<PathBuf>,
    initial_session: InitialSessionTarget<'_>,
    build_info: SupportBuildInfo,
    update_info: BuildMetadata,
) -> Result<()> {
    let cwd = env::current_dir()?;
    let config_path = preferred_config_path(config.as_deref(), &cwd)?;
    let (mut app, mut worker) = build_initial_app_with_session(
        cwd,
        config_path.clone(),
        RootConfig::load(&config_path),
        initial_session,
        spawn_worker,
    )?;
    app.set_support_build_info(build_info);
    app.set_update_build_info(update_info);

    enable_raw_mode()?;
    let inline_viewport_height = current_inline_viewport_height()?;
    let mut stdout = io::stdout();
    // Start from a clean screen: the inline viewport keeps prior terminal output above
    // the TUI, which would otherwise mix shell history with the interface.
    execute!(stdout, Clear(ClearType::All), MoveTo(0, 0))?;
    let mut cleanup = TerminalCleanupGuard::new();
    cleanup.raw_mode_enabled = true;
    let panic_hook = TuiPanicHookGuard::install();

    let keyboard_enhancement_enabled = enable_keyboard_enhancement_for_policy(
        app.terminal_keyboard_enhancement_policy(),
        &mut stdout,
    )?;
    app.set_terminal_keyboard_enhancement_enabled(keyboard_enhancement_enabled);
    cleanup.keyboard_enhancement_enabled = keyboard_enhancement_enabled;
    let bracketed_paste_enabled = enable_bracketed_paste(&mut stdout)?;
    cleanup.bracketed_paste_enabled = bracketed_paste_enabled;
    let mut focus_change_active = false;
    if app.terminal_notification_config().enabled {
        match execute!(stdout, EnableFocusChange) {
            Ok(()) => {
                focus_change_active = true;
                cleanup.focus_change_active = true;
            }
            Err(error) => {
                tracing::debug!(%error, "terminal focus reporting unavailable");
            }
        }
    }
    let mut mouse_capture_active = app.terminal_mouse_capture_enabled();
    if mouse_capture_active {
        execute!(stdout, EnableMouseCapture)?;
        cleanup.mouse_capture_active = true;
    }
    let mut terminal = terminal_with_inline_fallback(stdout, inline_viewport_height)?;
    let result = panic::catch_unwind(AssertUnwindSafe(
        || match tokio::runtime::Handle::try_current() {
            Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(|| {
                    handle.block_on(run_app(
                        &mut terminal,
                        &mut app,
                        &mut worker,
                        &mut mouse_capture_active,
                        &mut focus_change_active,
                    ))
                })
            }
            Ok(_) => anyhow::bail!(
                "the synchronous TUI launcher cannot run inside a current-thread Tokio runtime"
            ),
            Err(_) => {
                let event_runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                    .context("failed to build TUI event runtime")?;
                event_runtime.block_on(run_app(
                    &mut terminal,
                    &mut app,
                    &mut worker,
                    &mut mouse_capture_active,
                    &mut focus_change_active,
                ))
            }
        },
    ));
    cleanup.mouse_capture_active = mouse_capture_active;
    cleanup.focus_change_active = focus_change_active;
    let cleanup_result = cleanup.restore();
    panic_hook.restore();
    let result = match result {
        Ok(result) => result,
        Err(payload) => panic::resume_unwind(payload),
    };
    cleanup_result?;
    result?;
    print!("{}", render_tui_exit_resume_hint(&app, config.as_deref()));
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InitialSessionTarget<'a> {
    Fresh,
    Latest,
    Selector(&'a str),
}

#[cfg(not(test))]
struct TerminalCleanupGuard {
    raw_mode_enabled: bool,
    keyboard_enhancement_enabled: bool,
    bracketed_paste_enabled: bool,
    mouse_capture_active: bool,
    focus_change_active: bool,
}

#[cfg(not(test))]
impl TerminalCleanupGuard {
    fn new() -> Self {
        Self {
            raw_mode_enabled: false,
            keyboard_enhancement_enabled: false,
            bracketed_paste_enabled: false,
            mouse_capture_active: false,
            focus_change_active: false,
        }
    }

    fn restore(&mut self) -> io::Result<()> {
        let mut stdout = io::stdout();
        let mut first_error = None;
        if self.keyboard_enhancement_enabled {
            remember_cleanup_error(
                execute!(stdout, PopKeyboardEnhancementFlags),
                &mut first_error,
            );
            self.keyboard_enhancement_enabled = false;
        }
        if self.bracketed_paste_enabled {
            remember_cleanup_error(execute!(stdout, DisableBracketedPaste), &mut first_error);
            self.bracketed_paste_enabled = false;
        }
        if self.focus_change_active {
            remember_cleanup_error(execute!(stdout, DisableFocusChange), &mut first_error);
            self.focus_change_active = false;
        }
        if self.mouse_capture_active {
            remember_cleanup_error(execute!(stdout, DisableMouseCapture), &mut first_error);
            self.mouse_capture_active = false;
        }
        remember_cleanup_error(execute!(stdout, Show), &mut first_error);
        if self.raw_mode_enabled {
            remember_cleanup_error(disable_raw_mode(), &mut first_error);
            self.raw_mode_enabled = false;
        }
        first_error.map_or(Ok(()), Err)
    }
}

#[cfg(not(test))]
fn remember_cleanup_error(result: io::Result<()>, first_error: &mut Option<io::Error>) {
    if first_error.is_none()
        && let Err(error) = result
    {
        *first_error = Some(error);
    }
}

#[cfg(not(test))]
impl Drop for TerminalCleanupGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(not(test))]
type TuiPanicHook = dyn Fn(&panic::PanicHookInfo<'_>) + Send + Sync + 'static;

#[cfg(not(test))]
struct TuiPanicHookGuard {
    previous: Option<Arc<TuiPanicHook>>,
}

#[cfg(not(test))]
impl TuiPanicHookGuard {
    fn install() -> Self {
        let previous = Arc::<TuiPanicHook>::from(panic::take_hook());
        let hook_previous = Arc::clone(&previous);
        panic::set_hook(Box::new(move |info| {
            restore_terminal_escape_state();
            hook_previous(info);
        }));
        Self {
            previous: Some(previous),
        }
    }

    fn restore(mut self) {
        if let Some(previous) = self.previous.take() {
            panic::set_hook(Box::new(move |info| previous(info)));
        }
    }
}

#[cfg(not(test))]
impl Drop for TuiPanicHookGuard {
    fn drop(&mut self) {
        if std::thread::panicking() {
            return;
        }
        if let Some(previous) = self.previous.take() {
            panic::set_hook(Box::new(move |info| previous(info)));
        }
    }
}

#[cfg(not(test))]
fn restore_terminal_escape_state() {
    let mut stdout = io::stdout();
    let _ = execute!(stdout, PopKeyboardEnhancementFlags);
    let _ = execute!(stdout, DisableBracketedPaste);
    let _ = execute!(stdout, DisableFocusChange);
    let _ = execute!(stdout, DisableMouseCapture);
    let _ = execute!(stdout, Show);
    let _ = disable_raw_mode();
}

#[cfg(not(test))]
fn enable_keyboard_enhancement_for_policy<W: io::Write>(
    policy: TerminalKeyboardEnhancement,
    writer: &mut W,
) -> io::Result<bool> {
    match policy {
        TerminalKeyboardEnhancement::Auto => {
            if matches!(supports_keyboard_enhancement(), Ok(true)) {
                enable_keyboard_enhancement(writer)
            } else {
                Ok(false)
            }
        }
        TerminalKeyboardEnhancement::On => enable_keyboard_enhancement(writer),
        TerminalKeyboardEnhancement::Off => Ok(false),
    }
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
fn enable_bracketed_paste<W: io::Write>(writer: &mut W) -> io::Result<bool> {
    execute!(writer, EnableBracketedPaste)?;
    Ok(true)
}

#[cfg(not(test))]
fn current_inline_viewport_height() -> Result<u16> {
    let (_, height) = crossterm::terminal::size()?;
    Ok(height.max(12))
}

#[cfg(not(test))]
fn terminal_with_inline_fallback(
    stdout: io::Stdout,
    inline_viewport_height: u16,
) -> Result<TuiTerminal> {
    let backend = CrosstermBackend::new(stdout);
    let inline_result = CachedCursorBackend::capture(backend).and_then(|backend| {
        Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(inline_viewport_height),
            },
        )
    });
    inline_result.or_else(|inline_error| {
        let backend = CachedCursorBackend::with_position(
            CrosstermBackend::new(io::stdout()),
            ratatui::layout::Position::ORIGIN,
        );
        Terminal::new(backend).with_context(|| {
            format!(
                "inline viewport unavailable ({inline_error}); failed to initialize fallback terminal"
            )
        })
    })
}

#[cfg(not(test))]
async fn run_app(
    terminal: &mut TuiTerminal,
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    mouse_capture_active: &mut bool,
    focus_change_active: &mut bool,
) -> Result<()> {
    // From this point onward the event stream is the sole reader of terminal input. Capability
    // probes and the real cursor capture have already completed; ratatui resize reads are served
    // by the cached backend and transcript insertion uses scrolling regions.
    let mut terminal_events = EventStream::new();
    let mut scrollback = ScrollbackSyncState::default();
    let mut needs_render = true;
    let mut last_spinner_tick = live_spinner_tick();
    let mut latest_frame_area = Rect::default();
    let mut attention =
        AttentionController::from_current_process(app.terminal_notification_config());

    loop {
        attention.update_config(app.terminal_notification_config());
        let mut dirty = needs_render;
        dirty |= drain_worker_messages_with_attention(app, worker, &mut attention)?;
        dirty |= restart_worker_after_session_transition(app, worker, spawn_worker)?;
        attention.emit_pending_nonfatal(terminal.backend_mut());
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
        let focus_change_desired = app.terminal_notification_config().enabled;
        if *focus_change_active != focus_change_desired {
            let action = if focus_change_desired {
                execute!(terminal.backend_mut(), EnableFocusChange)
            } else {
                execute!(terminal.backend_mut(), DisableFocusChange)
            };
            match action {
                Ok(()) => {
                    *focus_change_active = focus_change_desired;
                    attention.reset_focus_reliability();
                }
                Err(error) => {
                    tracing::debug!(
                        %error,
                        enabled = focus_change_desired,
                        "failed to update terminal focus reporting"
                    );
                }
            }
        }

        let size = terminal.size()?;
        dirty |= app.set_terminal_size(size.width, size.height);
        dirty |= scrollback.has_pending_seed() && should_sync_terminal_scrollback(app);

        let spinner_tick = live_spinner_tick();
        if app.runtime.is_busy && spinner_tick != last_spinner_tick {
            dirty = true;
        }

        if dirty {
            sync_terminal_scrollback(terminal, app, &mut scrollback)?;
            terminal.draw(|frame| {
                latest_frame_area = frame.area();
                ui::render(frame, app);
            })?;
            let _ = app.maybe_start_automatic_update_check();
            let _ = app.acknowledge_active_egress_disclosure_frame();
            last_spinner_tick = spinner_tick;
            needs_render = false;
        }

        if app.should_quit {
            if let Some(runtime) = worker.as_ref() {
                let _ = runtime.worker_tx.send(AppState::shutdown_command());
            }
            break;
        }

        enum WakeEvent {
            Terminal(std::io::Result<CrosstermEvent>),
            Worker(Box<WorkerMessage>),
            WorkerClosed,
            Deadline,
        }
        let wake = {
            let terminal_event = terminal_events.next();
            let worker_message = next_worker_message(worker);
            match next_wake_deadline(app, &scrollback) {
                Some(deadline) => tokio::select! {
                    event = terminal_event => WakeEvent::Terminal(event.unwrap_or_else(|| {
                        Err(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "terminal event stream closed",
                        ))
                    })),
                    message = worker_message => message.map_or(
                        WakeEvent::WorkerClosed,
                        |message| WakeEvent::Worker(Box::new(message)),
                    ),
                    () = tokio::time::sleep(deadline) => WakeEvent::Deadline,
                },
                None => tokio::select! {
                    event = terminal_event => WakeEvent::Terminal(event.unwrap_or_else(|| {
                        Err(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "terminal event stream closed",
                        ))
                    })),
                    message = worker_message => message.map_or(
                        WakeEvent::WorkerClosed,
                        |message| WakeEvent::Worker(Box::new(message)),
                    ),
                },
            }
        };
        match wake {
            WakeEvent::Terminal(event) => {
                let mut next_event = Some(event?);
                for processed_events in 0..EVENT_BATCH_LIMIT {
                    let Some(crossterm_event) = next_event.take() else {
                        break;
                    };
                    let break_batch = process_terminal_event(
                        terminal,
                        app,
                        worker,
                        &mut needs_render,
                        latest_frame_area,
                        size.into(),
                        crossterm_event,
                        &mut attention,
                        spawn_worker,
                    )?;
                    if break_batch || processed_events + 1 >= EVENT_BATCH_LIMIT {
                        break;
                    }
                    next_event = event::poll(Duration::ZERO)?.then(event::read).transpose()?;
                }
            }
            WakeEvent::Worker(message) => {
                needs_render |=
                    apply_received_worker_message(app, worker, &mut attention, *message)?;
            }
            WakeEvent::WorkerClosed => {
                *worker = None;
                app.handle_worker_message(WorkerMessage::RunFailed(
                    "agent worker disconnected".to_owned(),
                ))?;
                needs_render = true;
            }
            WakeEvent::Deadline => {}
        }
    }

    Ok(())
}

#[cfg(not(test))]
fn process_terminal_event<F>(
    terminal: &mut TuiTerminal,
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    needs_render: &mut bool,
    latest_frame_area: Rect,
    terminal_size: Rect,
    crossterm_event: CrosstermEvent,
    attention: &mut AttentionController,
    spawn_worker: F,
) -> Result<bool>
where
    F: FnMut(RootConfig, &AppState) -> Result<WorkerRuntime>,
{
    match crossterm_event {
        CrosstermEvent::Resize(_, _) => {
            terminal.autoresize()?;
            *needs_render = true;
            Ok(true)
        }
        CrosstermEvent::Mouse(mouse) => {
            let layout = mouse_layout_snapshot(latest_frame_area, terminal_size, app);
            let outcome = app.handle_mouse_event(mouse.into(), &layout)?;
            *needs_render |= apply_mouse_outcome(app, worker, outcome, spawn_worker)?;
            Ok(false)
        }
        CrosstermEvent::Paste(text) => {
            app.handle_paste_text(&text);
            *needs_render = true;
            Ok(false)
        }
        CrosstermEvent::FocusGained => {
            attention.observe_focus(true);
            Ok(false)
        }
        CrosstermEvent::FocusLost => {
            attention.observe_focus(false);
            Ok(false)
        }
        CrosstermEvent::Key(key) if key.kind == KeyEventKind::Press => {
            if matches!(key.code, KeyCode::Char('v') | KeyCode::Char('V'))
                && key.modifiers == KeyModifiers::CONTROL
                && app.can_accept_image_attachment_input()
            {
                match crate::clipboard_image::read_clipboard_image_png() {
                    Ok(Some(encoded_png)) => {
                        app.handle_clipboard_image(encoded_png);
                        *needs_render = true;
                        return Ok(false);
                    }
                    Ok(None) => {}
                    Err(_) => {
                        app.report_clipboard_image_failure();
                        *needs_render = true;
                        return Ok(false);
                    }
                }
            }
            let action = app.handle_key_event(key)?;
            *needs_render |= apply_key_action(app, worker, action, spawn_worker)?;
            Ok(false)
        }
        _ => Ok(false),
    }
}

fn mouse_layout_snapshot(frame_area: Rect, terminal_size: Rect, app: &AppState) -> LayoutSnapshot {
    let screen = if frame_area.width == 0 || frame_area.height == 0 {
        Rect::new(0, 0, terminal_size.width, terminal_size.height)
    } else {
        frame_area
    };
    LayoutSnapshot::from_app(screen, app)
}

#[cfg(test)]
fn build_initial_app<F>(
    cwd: PathBuf,
    config_path: PathBuf,
    load_result: Result<RootConfig>,
    spawn_worker_fn: F,
) -> Result<(AppState, Option<WorkerRuntime>)>
where
    F: FnMut(RootConfig, &AppState) -> Result<WorkerRuntime>,
{
    build_initial_app_with_session(
        cwd,
        config_path,
        load_result,
        InitialSessionTarget::Fresh,
        spawn_worker_fn,
    )
}

fn build_initial_app_with_session<F>(
    cwd: PathBuf,
    config_path: PathBuf,
    load_result: Result<RootConfig>,
    initial_session: InitialSessionTarget<'_>,
    mut spawn_worker_fn: F,
) -> Result<(AppState, Option<WorkerRuntime>)>
where
    F: FnMut(RootConfig, &AppState) -> Result<WorkerRuntime>,
{
    let mut worker = None;
    let app = match load_result {
        Ok(root_config) => {
            let mut app = AppState::from_root_config(&config_path, &root_config);
            if app.workspace_is_trusted_from_history() {
                restore_initial_session_from_disk(&mut app, &root_config, initial_session)?;
                let trust_ready = match app.ensure_current_workspace_trust_decision(
                    "trusted workspace carried into session",
                ) {
                    Ok(()) => true,
                    Err(error) => {
                        if let Some(recovery) = runner::worker_session_route_recovery_message(
                            &error,
                            &app.session_log_path,
                        ) {
                            app.handle_worker_message(recovery)?;
                        } else {
                            app.handle_worker_message(
                                WorkerMessage::SessionRouteRecoveryRequired {
                                    code:
                                        sigil_kernel::PublicRouteRecoveryCode::SessionStreamInvalid,
                                    actions: vec![
                                    sigil_kernel::PublicRouteRecoveryAction::StartNewSession,
                                    sigil_kernel::PublicRouteRecoveryAction::BackToSessionLibrary,
                                ],
                                    recovery_binding: String::new(),
                                    retryable: false,
                                    target_session: None,
                                },
                            )?;
                        }
                        false
                    }
                };
                if trust_ready {
                    match spawn_worker_fn(root_config, &app) {
                        Ok(runtime) => worker = Some(runtime),
                        Err(error) => {
                            if let Some(recovery) = runner::worker_session_route_recovery_message(
                                &error,
                                &app.session_log_path,
                            ) {
                                app.handle_worker_message(recovery)?;
                            } else {
                                let message = format!(
                                    "agent runtime is unavailable; session controls remain available: {error:#}"
                                );
                                report_worker_unavailable(&mut app, &message)?;
                            }
                        }
                    }
                }
                flush_pending_worker_commands(&mut app, &mut worker)?;
            } else {
                app.enter_workspace_trust_gate()?;
            }
            app
        }
        Err(error) => {
            let startup_error = config_path.exists().then(|| error.to_string());
            AppState::from_setup(config_path.clone(), cwd, startup_error)
        }
    };
    Ok((app, worker))
}

fn restore_initial_session_from_disk(
    app: &mut AppState,
    root_config: &RootConfig,
    initial_session: InitialSessionTarget<'_>,
) -> Result<()> {
    match initial_session {
        InitialSessionTarget::Fresh => Ok(()),
        InitialSessionTarget::Latest => {
            app.restore_latest_session_from_disk(root_config);
            Ok(())
        }
        InitialSessionTarget::Selector(selector) => {
            let selector = selector.trim();
            let selector = if selector.is_empty() {
                "latest"
            } else {
                selector
            };
            if app.restore_session_selector_from_disk(
                selector,
                &root_config.agent.runtime_provider,
                &root_config.agent.model,
                "restored requested session",
            )? {
                Ok(())
            } else {
                Err(anyhow::anyhow!("no saved session matches {selector}"))
            }
        }
    }
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
            let support_build_info = app.support_build_info().clone();
            let update_build_info = app.update_build_info().clone();
            *app = AppState::from_root_config(&config_path, &root_config);
            app.set_support_build_info(support_build_info);
            app.set_update_build_info(update_build_info);
            if let Err(error) =
                app.ensure_current_workspace_trust_decision("trusted by user during quick setup")
            {
                apply_worker_startup_recovery(app, &error, &app.session_log_path.clone())?;
                return Ok(());
            }
            match spawn_worker_fn(*root_config, app) {
                Ok(runtime) => *worker = Some(runtime),
                Err(error) => report_worker_unavailable(
                    app,
                    &format!("setup completed; agent runtime remains unavailable: {error:#}"),
                )?,
            }
        }
        AppAction::TrustWorkspace => {
            if let Err(error) = app.confirm_workspace_trust_gate() {
                apply_worker_startup_recovery(app, &error, &app.session_log_path.clone())?;
                return Ok(());
            }
            shutdown_and_join_worker(worker);
            let Some(root_config) = app.root_config_snapshot().cloned() else {
                report_worker_unavailable(app, "agent worker stopped; runtime config unavailable")?;
                return Ok(());
            };
            match spawn_worker_fn(root_config, app) {
                Ok(runtime) => *worker = Some(runtime),
                Err(error) => report_worker_unavailable(
                    app,
                    &format!("workspace trusted; agent runtime remains unavailable: {error:#}"),
                )?,
            }
        }
        AppAction::ConfigSaved { root_config }
        | AppAction::RuntimeConfigUpdated { root_config } => {
            let Some(runtime_config) = app.runtime_config_for_current_session(*root_config)? else {
                return Ok(());
            };
            shutdown_and_join_worker(worker);
            match spawn_worker_fn(runtime_config, app) {
                Ok(runtime) => *worker = Some(runtime),
                Err(error) => report_worker_unavailable(
                    app,
                    &format!("configuration saved; agent runtime remains unavailable: {error:#}"),
                )?,
            }
        }
        AppAction::UpdateActiveRunPermissionMode { mode } => {
            if let Some(runtime) = worker.as_ref() {
                let _ = runtime
                    .worker_tx
                    .send(super::runner::WorkerCommand::UpdateActiveRunPermissionMode { mode });
            }
        }
        AppAction::SetDefaultModel {
            root_config,
            expected_root_config,
        } => {
            root_config.save_if_unchanged(&app.config_path, &expected_root_config)?;
            app.apply_saved_default_model(*root_config);
        }
        AppAction::StartNewSession { session_log_path } => {
            if let Some(runtime) = worker.as_ref()
                && runtime
                    .worker_tx
                    .send(WorkerCommand::StartNewSession {
                        session_log_path: session_log_path.clone(),
                    })
                    .is_ok()
            {
                return Ok(());
            }
            let root_config = app
                .root_config_snapshot()
                .cloned()
                .context("new session requires the current runtime config")?;
            let preparation = (|| -> Result<_> {
                let (_, fallback_route) =
                    sigil_runtime::provider_connections::resolve_default_model_route(&root_config)
                        .map_err(anyhow::Error::new)?;
                let attachment = Arc::new(
                    sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
                        &session_log_path,
                    )
                    .map_err(anyhow::Error::new)?,
                );
                let session = sigil_runtime::provider_connections::load_session_for_route_resume_with_directive_and_attachment(
                    &root_config,
                    &fallback_route,
                    JsonlSessionStore::new(&session_log_path)?,
                    None,
                    None,
                    Some(attachment.as_ref()),
                )
                .map_err(anyhow::Error::new)?;
                Ok((attachment, session))
            })();
            let (attachment, session) = match preparation {
                Ok(prepared) => prepared,
                Err(error) => {
                    apply_worker_startup_recovery(app, &error, &session_log_path)?;
                    return Ok(());
                }
            };
            let provider_name = session.provider_name().to_owned();
            let model_name = session.model_name().to_owned();
            let mut entries = session.entries().to_vec();
            if let Err(error) = app.ensure_target_workspace_trust_decision_with_attachment(
                &session_log_path,
                &mut entries,
                "trusted workspace carried into new session",
                attachment.as_ref(),
            ) {
                apply_worker_startup_recovery(app, &error, &session_log_path)?;
                return Ok(());
            }
            shutdown_and_join_worker(worker);
            app.handle_worker_message(WorkerMessage::NewSessionStarted {
                session_log_path: session_log_path.clone(),
                provider_name,
                model_name,
                entries,
            })?;
            let _ = app.take_worker_rebind_required();
            app.retain_worker_session_attachment(session_log_path, attachment);
            match spawn_worker_fn(root_config, app) {
                Ok(runtime) => *worker = Some(runtime),
                Err(error) => report_worker_unavailable(
                    app,
                    &format!("new session opened but its agent worker is unavailable: {error:#}"),
                )?,
            }
        }
        AppAction::SwitchSession { session_log_path } => {
            let attachment_recovery_binding = app
                .pending_session_attachment_recovery_binding_for(&session_log_path)
                .map(str::to_owned);
            app.mark_pending_session_transition_target(session_log_path.clone());
            if let Some(runtime) = worker.as_ref()
                && runtime
                    .worker_tx
                    .send(WorkerCommand::SwitchSession {
                        session_log_path: session_log_path.clone(),
                        attachment_recovery_binding: attachment_recovery_binding.clone(),
                    })
                    .is_ok()
            {
                return Ok(());
            }
            let root_config = app
                .root_config_snapshot()
                .cloned()
                .context("session switch requires the current runtime config")?;
            let preparation = (|| -> Result<_> {
                let (_, fallback_route) =
                    sigil_runtime::provider_connections::resolve_default_model_route(&root_config)
                        .map_err(anyhow::Error::new)?;
                let target_attachment = Arc::new(
                    if let Some(recovery_binding) = attachment_recovery_binding.as_deref() {
                        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire_for_path_retry(
                            &session_log_path,
                            recovery_binding,
                        )
                    } else {
                        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
                            &session_log_path,
                        )
                    }
                    .map_err(anyhow::Error::new)?,
                );
                let target = sigil_runtime::provider_connections::load_session_for_route_resume_with_directive_and_attachment(
                    &root_config,
                    &fallback_route,
                    JsonlSessionStore::new(&session_log_path)?,
                    app.pending_session_route_confirmation_binding(),
                    None,
                    Some(target_attachment.as_ref()),
                )
                .map_err(anyhow::Error::new)?;
                Ok((target_attachment, target))
            })();
            let (target_attachment, target) = match preparation {
                Ok(prepared) => prepared,
                Err(error) => {
                    apply_worker_startup_recovery(app, &error, &session_log_path)?;
                    return Ok(());
                }
            };
            let provider_name = target.provider_name().to_owned();
            let model_name = target.model_name().to_owned();
            let mut entries = target.entries().to_vec();
            if let Err(error) = app.ensure_target_workspace_trust_decision_with_attachment(
                &session_log_path,
                &mut entries,
                "trusted workspace carried into restored session",
                target_attachment.as_ref(),
            ) {
                apply_worker_startup_recovery(app, &error, &session_log_path)?;
                return Ok(());
            }
            shutdown_and_join_worker(worker);
            app.handle_worker_message(WorkerMessage::SessionSwitched {
                session_log_path: session_log_path.clone(),
                provider_name,
                model_name,
                entries,
            })?;
            app.retain_worker_session_attachment(session_log_path, target_attachment);
            match spawn_worker_fn(root_config, app) {
                Ok(runtime) => *worker = Some(runtime),
                Err(error) => report_worker_unavailable(
                    app,
                    &format!(
                        "session opened read-only; its agent worker is unavailable: {error:#}"
                    ),
                )?,
            }
        }
        AppAction::CopyToClipboard { text } => {
            match clipboard::copy_text(&text, app.terminal_osc52_clipboard_enabled()) {
                ClipboardCopyOutcome::Copied => app.record_clipboard_copy_success(&text),
                ClipboardCopyOutcome::Unavailable(reason) => {
                    app.record_clipboard_copy_unavailable(&reason);
                }
            }
        }
        AppAction::CopySecretToClipboard { text } => {
            match clipboard::copy_text(text.expose_secret(), app.terminal_osc52_clipboard_enabled())
            {
                ClipboardCopyOutcome::Copied => {
                    app.record_clipboard_copy_success(text.expose_secret());
                }
                ClipboardCopyOutcome::Unavailable(reason) => {
                    app.record_clipboard_copy_unavailable(&reason);
                }
            }
        }
        AppAction::OpenExternalUrl { url } => {
            match launch_external_target(ExternalLaunchTarget::Url(&url)) {
                Ok(()) => app.record_feedback_external_action_success("opening bug report form"),
                Err(error) => {
                    app.record_feedback_external_action_failure("open bug report form", &error);
                }
            }
        }
        AppAction::OpenSecretExternalUrl { url } => {
            match launch_external_target(ExternalLaunchTarget::Url(url.expose_secret())) {
                Ok(()) => app.record_feedback_external_action_success("opening authorization page"),
                Err(error) => {
                    app.record_feedback_external_action_failure("open authorization page", &error);
                }
            }
        }
        AppAction::RevealFile { path } => {
            match launch_external_target(ExternalLaunchTarget::RevealFile(&path)) {
                Ok(()) => app.record_feedback_external_action_success("revealing feedback report"),
                Err(error) => {
                    app.record_feedback_external_action_failure("reveal feedback report", &error);
                }
            }
        }
        AppAction::CheckForUpdate {
            force_refresh,
            channel,
        } => {
            app.start_update_check(force_refresh, true, channel);
        }
        AppAction::ApplyUpdate { channel } => {
            app.start_update_apply(channel);
        }
        action => {
            let command = app.into_worker_command(action);
            send_worker_command_with_restart(app, worker, command, &mut spawn_worker_fn)?;
        }
    }
    flush_pending_worker_commands(app, worker)?;
    Ok(())
}

fn apply_worker_startup_recovery(
    app: &mut AppState,
    error: &anyhow::Error,
    session_log_path: &Path,
) -> Result<()> {
    let recovery = runner::worker_session_route_recovery_message(error, session_log_path)
        .unwrap_or_else(|| WorkerMessage::SessionRouteRecoveryRequired {
            code: sigil_kernel::PublicRouteRecoveryCode::SessionStreamInvalid,
            actions: vec![
                sigil_kernel::PublicRouteRecoveryAction::StartNewSession,
                sigil_kernel::PublicRouteRecoveryAction::BackToSessionLibrary,
            ],
            recovery_binding: String::new(),
            retryable: false,
            target_session: None,
        });
    app.handle_worker_message(recovery)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExternalLaunchPlatform {
    MacOs,
    Windows,
    Freedesktop,
    Unsupported,
}

#[derive(Debug, Clone, Copy)]
enum ExternalLaunchTarget<'a> {
    Url(&'a str),
    RevealFile(&'a Path),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExternalLaunchPlan {
    program: &'static str,
    args: Vec<OsString>,
}

fn external_launch_plan(
    target: ExternalLaunchTarget<'_>,
    platform: ExternalLaunchPlatform,
) -> Result<ExternalLaunchPlan> {
    if let ExternalLaunchTarget::Url(url) = target
        && (!url.starts_with("https://") || url.chars().any(char::is_control))
    {
        anyhow::bail!("external URL must be a valid HTTPS URL");
    }

    let plan = match (platform, target) {
        (ExternalLaunchPlatform::MacOs, ExternalLaunchTarget::Url(url)) => ExternalLaunchPlan {
            program: "/usr/bin/open",
            args: vec![OsString::from(url)],
        },
        (ExternalLaunchPlatform::MacOs, ExternalLaunchTarget::RevealFile(path)) => {
            ExternalLaunchPlan {
                program: "/usr/bin/open",
                args: vec![OsString::from("-R"), path.as_os_str().to_owned()],
            }
        }
        (ExternalLaunchPlatform::Windows, ExternalLaunchTarget::Url(url)) => ExternalLaunchPlan {
            program: "rundll32.exe",
            args: vec![
                OsString::from("url.dll,FileProtocolHandler"),
                OsString::from(url),
            ],
        },
        (ExternalLaunchPlatform::Windows, ExternalLaunchTarget::RevealFile(path)) => {
            let mut select_arg = OsString::from("/select,");
            select_arg.push(path.as_os_str());
            ExternalLaunchPlan {
                program: "explorer.exe",
                args: vec![select_arg],
            }
        }
        (ExternalLaunchPlatform::Freedesktop, ExternalLaunchTarget::Url(url)) => {
            ExternalLaunchPlan {
                program: "xdg-open",
                args: vec![OsString::from(url)],
            }
        }
        (ExternalLaunchPlatform::Freedesktop, ExternalLaunchTarget::RevealFile(path)) => {
            let parent = path
                .parent()
                .ok_or_else(|| anyhow::anyhow!("support report has no parent directory"))?;
            ExternalLaunchPlan {
                program: "xdg-open",
                args: vec![parent.as_os_str().to_owned()],
            }
        }
        (ExternalLaunchPlatform::Unsupported, _) => {
            anyhow::bail!("external opening is not supported on this platform");
        }
    };
    Ok(plan)
}

const fn current_external_launch_platform() -> ExternalLaunchPlatform {
    if cfg!(target_os = "macos") {
        ExternalLaunchPlatform::MacOs
    } else if cfg!(target_os = "windows") {
        ExternalLaunchPlatform::Windows
    } else if cfg!(unix) {
        ExternalLaunchPlatform::Freedesktop
    } else {
        ExternalLaunchPlatform::Unsupported
    }
}

#[cfg(not(test))]
fn launch_external_target(target: ExternalLaunchTarget<'_>) -> Result<()> {
    let plan = external_launch_plan(target, current_external_launch_platform())?;
    Command::new(plan.program)
        .args(plan.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to launch {}", plan.program))?;
    Ok(())
}

#[cfg(test)]
fn launch_external_target(target: ExternalLaunchTarget<'_>) -> Result<()> {
    external_launch_plan(target, current_external_launch_platform()).map(|_| ())
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

#[cfg(test)]
fn drain_worker_messages(app: &mut AppState, worker: &mut Option<WorkerRuntime>) -> Result<bool> {
    drain_worker_messages_inner(app, worker, None)
}

#[cfg(not(test))]
fn drain_worker_messages_with_attention(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    attention: &mut AttentionController,
) -> Result<bool> {
    drain_worker_messages_inner(app, worker, Some(attention))
}

fn drain_worker_messages_inner(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    mut attention: Option<&mut AttentionController>,
) -> Result<bool> {
    let Some(runtime) = worker.as_mut() else {
        return Ok(false);
    };
    let mut dirty = false;
    let mut startup_failed = false;
    app.begin_timeline_render_batch();
    while let Some(message) = try_recv_worker_message(runtime) {
        startup_failed |= apply_worker_message_state(runtime, attention.as_deref_mut(), &message);
        app.handle_worker_message(message)?;
        dirty = true;
    }
    if startup_failed {
        shutdown_and_join_worker(worker);
    }
    Ok(dirty | app.flush_timeline_render_batch())
}

fn try_recv_worker_message(runtime: &mut WorkerRuntime) -> Option<WorkerMessage> {
    #[cfg(test)]
    {
        runtime.worker_rx.try_recv().ok()
    }
    #[cfg(not(test))]
    {
        runtime.worker_rx.try_recv()
    }
}

#[cfg(not(test))]
async fn next_worker_message(worker: &mut Option<WorkerRuntime>) -> Option<WorkerMessage> {
    match worker.as_mut() {
        Some(runtime) => runtime.worker_rx.recv().await,
        None => std::future::pending().await,
    }
}

#[cfg(not(test))]
fn apply_received_worker_message(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    attention: &mut AttentionController,
    message: WorkerMessage,
) -> Result<bool> {
    let Some(runtime) = worker.as_mut() else {
        return Ok(false);
    };
    app.begin_timeline_render_batch();
    let startup_failed = apply_worker_message_state(runtime, Some(attention), &message);
    app.handle_worker_message(message)?;
    app.flush_timeline_render_batch();
    if startup_failed {
        shutdown_and_join_worker(worker);
    }
    Ok(true)
}

fn apply_worker_message_state(
    runtime: &mut WorkerRuntime,
    attention: Option<&mut AttentionController>,
    message: &WorkerMessage,
) -> bool {
    let route_transition_recovery = matches!(
        message,
        WorkerMessage::SessionRouteRecoveryRequired {
            target_session: Some(_),
            ..
        }
    );
    let startup_failed = route_transition_recovery
        || (!runtime.ready
            && matches!(
                message,
                WorkerMessage::RunFailed(_) | WorkerMessage::SessionRouteRecoveryRequired { .. }
            ));
    if matches!(message, WorkerMessage::WorkerReady) {
        runtime.ready = true;
    }
    if let Some(attention) = attention {
        attention.observe(message, Instant::now());
    }
    startup_failed
}

fn restart_worker_after_session_transition<F>(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    mut spawn_worker_fn: F,
) -> Result<bool>
where
    F: FnMut(RootConfig, &AppState) -> Result<WorkerRuntime>,
{
    if !app.take_worker_rebind_required() {
        return Ok(false);
    }
    shutdown_and_join_worker(worker);
    let Some(root_config) = app.root_config_snapshot().cloned() else {
        report_worker_unavailable(
            app,
            "session changed but the runtime config is unavailable; no prompt was sent",
        )?;
        return Ok(true);
    };
    match spawn_worker_fn(root_config, app) {
        Ok(runtime) => *worker = Some(runtime),
        Err(error) => report_worker_unavailable(
            app,
            &format!("session changed but the agent worker could not rebind: {error:#}"),
        )?,
    }
    Ok(true)
}

fn flush_pending_worker_commands(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
) -> Result<bool> {
    if !app.has_pending_worker_commands() {
        return Ok(false);
    }
    let Some(runtime) = worker.as_ref() else {
        return Ok(false);
    };
    if !runtime.ready {
        return Ok(false);
    }
    let commands = app.drain_pending_worker_commands();
    let dirty = !commands.is_empty();
    let mut commands = commands.into_iter();
    while let Some(command) = commands.next() {
        let Some(runtime) = worker.as_ref() else {
            app.enqueue_worker_command(command);
            for remaining in commands {
                app.enqueue_worker_command(remaining);
            }
            break;
        };
        if let Err(error) = runtime.worker_tx.send(command) {
            app.enqueue_worker_command(*error.0);
            for remaining in commands {
                app.enqueue_worker_command(remaining);
            }
            shutdown_and_join_worker(worker);
            report_worker_unavailable(app, "agent worker stopped before accepting command")?;
            break;
        }
    }
    Ok(dirty)
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
        if !runtime.ready {
            app.enqueue_worker_command(command);
            return Ok(());
        }
        match runtime.worker_tx.send(command) {
            Ok(()) => return Ok(()),
            Err(error) => {
                let command = *error.0;
                shutdown_and_join_worker(worker);
                command
            }
        }
    } else {
        command
    };

    let Some(root_config) = app.root_config_snapshot().cloned() else {
        app.enqueue_worker_command(command);
        report_worker_unavailable(app, "agent worker stopped; runtime config unavailable")?;
        return Ok(());
    };

    match spawn_worker_fn(root_config, app) {
        Ok(runtime) => {
            *worker = Some(runtime);
        }
        Err(error) => {
            app.enqueue_worker_command(command);
            report_worker_unavailable(app, &format!("failed to restart agent worker: {error:#}"))?;
            return Ok(());
        }
    }

    if let Some(runtime) = worker.as_ref() {
        if !runtime.ready {
            app.enqueue_worker_command(command);
            return Ok(());
        }
        match runtime.worker_tx.send(command) {
            Ok(()) => return Ok(()),
            Err(error) => app.enqueue_worker_command(*error.0),
        }
    }
    shutdown_and_join_worker(worker);
    report_worker_unavailable(app, "agent worker stopped before accepting command")
}

fn report_worker_unavailable(app: &mut AppState, message: &str) -> Result<()> {
    app.handle_worker_message(WorkerMessage::Notice(message.to_owned()))?;
    app.handle_worker_message(WorkerMessage::SessionRouteRecoveryRequired {
        code: sigil_kernel::PublicRouteRecoveryCode::ProviderUnavailable,
        actions: vec![
            sigil_kernel::PublicRouteRecoveryAction::RetryProvider,
            sigil_kernel::PublicRouteRecoveryAction::RepairConnection,
            sigil_kernel::PublicRouteRecoveryAction::StartNewSession,
            sigil_kernel::PublicRouteRecoveryAction::BackToSessionLibrary,
        ],
        recovery_binding: String::new(),
        retryable: true,
        target_session: None,
    })
}

fn shutdown_and_join_worker(worker: &mut Option<WorkerRuntime>) {
    let Some(runtime) = worker.take() else {
        return;
    };
    let _ = runtime.worker_tx.send(AppState::shutdown_command());
    #[cfg(not(test))]
    {
        let mut runtime = runtime;
        if let Some(join_handle) = runtime.join_handle.take() {
            let _ = join_handle.join();
        }
    }
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
    terminal: &mut TuiTerminal,
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
    !app.runtime.is_busy && !app.is_setup_mode() && !app.is_config_mode()
}

fn next_wake_deadline(app: &AppState, scrollback: &ScrollbackSyncState) -> Option<Duration> {
    if app.runtime.is_busy {
        Some(Duration::from_millis(SPINNER_FRAME_MILLIS as u64))
    } else if scrollback.has_pending_seed() && should_sync_terminal_scrollback(app) {
        Some(SCROLLBACK_SEED_POLL_INTERVAL)
    } else if app.has_pending_background_tasks() {
        Some(BACKGROUND_TASK_WAKE_INTERVAL)
    } else {
        None
    }
}

#[cfg(test)]
fn tested_next_wake_deadline(app: &AppState, scrollback: &ScrollbackSyncState) -> Option<Duration> {
    next_wake_deadline(app, scrollback)
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
        return ScrollbackSyncPlan::Noop;
    }
    if next_line_count > sync_state.line_count && shared_hash == sync_state.sequence_hash {
        return ScrollbackSyncPlan::Append {
            from_index: sync_state.line_count,
        };
    }
    ScrollbackSyncPlan::Noop
}

#[cfg(not(test))]
fn insert_scrollback_lines(terminal: &mut TuiTerminal, lines: Vec<Line<'static>>) -> Result<()> {
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
    let theme = theme::resolve_for_app(app);
    let palette = &theme.palette;
    Line::from(vec![
        Span::styled("---- session ", Style::default().fg(palette.text_muted)),
        Span::styled(
            app.session_id.chars().take(8).collect::<String>(),
            Style::default()
                .fg(palette.accent_info)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                " {} / {} ----",
                app.runtime.provider_name, app.runtime.model_name
            ),
            Style::default().fg(palette.text_muted),
        ),
    ])
}

fn render_tui_exit_resume_hint(app: &AppState, explicit_config: Option<&Path>) -> String {
    if app.is_setup_mode() || app.is_workspace_trust_gate_mode() {
        return String::new();
    }
    let mut command = String::from("sigil");
    if let Some(config) = explicit_config {
        command.push_str(" --config ");
        command.push_str(&shell_quote(&config.display().to_string()));
    }
    command.push_str(" resume ");
    command.push_str(&shell_quote(&app.session_id));
    format!(
        "Sigil session: {}\nResume with: {command}\n",
        app.session_id
    )
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_owned();
    }
    if value.chars().all(|ch| {
        ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '=' | '+')
    }) {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

struct WorkerRuntime {
    worker_tx: runner::WorkerCommandSender,
    #[cfg(test)]
    worker_rx: std::sync::mpsc::Receiver<WorkerMessage>,
    #[cfg(not(test))]
    worker_rx: WorkerMessageInbox,
    #[cfg(not(test))]
    join_handle: Option<std::thread::JoinHandle<()>>,
    ready: bool,
}

#[cfg(not(test))]
struct WorkerMessageInbox {
    receiver: tokio::sync::mpsc::UnboundedReceiver<WorkerMessage>,
}

#[cfg(not(test))]
impl WorkerMessageInbox {
    fn forward_from(receiver: std::sync::mpsc::Receiver<WorkerMessage>) -> Result<Self> {
        let (sender, forwarded) = tokio::sync::mpsc::unbounded_channel();
        std::thread::Builder::new()
            .name("sigil-tui-worker-events".to_owned())
            .spawn(move || {
                while let Ok(message) = receiver.recv() {
                    if sender.send(message).is_err() {
                        break;
                    }
                }
            })
            .context("failed to start TUI worker event forwarder")?;
        Ok(Self {
            receiver: forwarded,
        })
    }

    fn try_recv(&mut self) -> Option<WorkerMessage> {
        self.receiver.try_recv().ok()
    }

    async fn recv(&mut self) -> Option<WorkerMessage> {
        self.receiver.recv().await
    }
}

#[cfg(not(test))]
fn spawn_worker(root_config: RootConfig, app: &AppState) -> Result<WorkerRuntime> {
    let spawned = runner::spawn_agent_worker_with_route_directive_and_attachment(
        root_config,
        app.config_path.clone(),
        app.session_log_path.clone(),
        app.workspace_root.clone(),
        sigil_kernel::InteractionMode::Interactive,
        runner::WorkerSessionRouteDirective {
            recovery_confirmation: app
                .pending_session_route_confirmation_binding()
                .map(str::to_owned),
            explicit_selection: app.pending_session_route_selection().cloned(),
        },
        app.worker_session_attachment(),
    )?;
    Ok(WorkerRuntime {
        worker_tx: spawned.command_tx,
        worker_rx: WorkerMessageInbox::forward_from(spawned.message_rx)?,
        join_handle: Some(spawned.join_handle),
        ready: false,
    })
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/main_tests.rs"]
mod tests;
