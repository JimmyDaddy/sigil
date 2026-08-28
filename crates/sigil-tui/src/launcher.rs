use std::{
    panic,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

#[cfg(not(test))]
use std::{
    env, io,
    panic::AssertUnwindSafe,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use crossterm::{
    cursor::MoveTo,
    execute,
    terminal::{Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
#[cfg(not(test))]
use crossterm::{
    cursor::Show,
    event::{
        self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture, Event as CrosstermEvent, EventStream,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    terminal::{disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement},
};
#[cfg(not(test))]
use futures::StreamExt;
#[cfg(not(test))]
use ratatui::backend::CrosstermBackend;
#[cfg(test)]
use ratatui::layout::Rect;
use ratatui::{Terminal, backend::Backend, layout::Position};
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
use crate::application_bridge;
#[cfg(not(test))]
use crate::host_effects::SystemHostEffects;
#[cfg(test)]
use crate::host_effects::{ExternalLaunchPlatform, TestHostEffects, external_launch_plan};
#[cfg(not(test))]
use crate::input_event::{FocusChange, InputEvent, InputKeyCode, InputKeyEventKind, Modifiers};
use crate::ui;
#[cfg(test)]
use crate::ui::LayoutSnapshot;
use crate::{
    app::{AppAction, AppState},
    attention::AttentionController,
    damage::Damage,
    host_effects::{ExternalLaunchTarget, HostEffects},
    input_event::{EventEffect, HostRequest},
    mouse::AppMouseOutcome,
    presentation::PresentationSession,
    runner::{self, WorkerCommand, WorkerMessage},
    surface_adapter::build_surface_model,
};

const BACKGROUND_TASK_WAKE_INTERVAL: Duration = Duration::from_millis(250);
#[cfg(not(test))]
const EVENT_BATCH_LIMIT: usize = 64;
const SPINNER_FRAME_MILLIS: u128 = 120;

#[cfg(not(test))]
type TuiTerminal = Terminal<CrosstermBackend<io::Stdout>>;

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
    let boot_result =
        sigil_runtime::r71_authority_composition::boot_current_schema(&config_path, &cwd)
            .map_err(anyhow::Error::new);
    let (mut app, mut worker) = build_initial_app_with_session(
        cwd,
        config_path.clone(),
        boot_result,
        initial_session,
        spawn_worker,
    )?;
    app.set_support_build_info(build_info);
    app.set_update_build_info(update_info);

    let mut cleanup = TerminalCleanupGuard::new();
    let (panic_hook, mut background_panics) = TuiPanicHookGuard::install();
    enable_raw_mode()?;
    cleanup.raw_mode_enabled = true;
    let mut stdout = io::stdout();
    enter_terminal_presentation(&mut stdout)?;
    cleanup.alternate_screen_active = true;

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
    let mut terminal = terminal_fullscreen(stdout)?;
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
                        &mut background_panics,
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
                    &mut background_panics,
                ))
            }
        },
    ));
    let background_panic_during_shutdown = if result.is_ok() {
        shutdown_and_join_worker(&mut worker);
        app.release_worker_session_attachment();
        background_panics.try_recv().ok()
    } else {
        None
    };
    let result = match (result, background_panic_during_shutdown) {
        (Ok(Ok(())), Some(report)) => Ok(Err(anyhow::anyhow!(report))),
        (result, _) => result,
    };
    let clean_exit = matches!(&result, Ok(Ok(())));
    if clean_exit {
        let _ = app.discard_current_bootstrap_only_session();
    }
    cleanup.mouse_capture_active = mouse_capture_active;
    cleanup.focus_change_active = focus_change_active;
    // The panic hook has already restored the primary screen before catch_unwind observes the
    // payload. Clearing through the stale Terminal in that case would erase the panic report from
    // the primary screen. Ordinary Result errors have not run the hook and still need finalizing.
    let presentation_cleanup_result = if result.is_err() {
        Ok(())
    } else {
        finalize_terminal_presentation(&mut terminal)
    };
    let cleanup_result = cleanup.restore();
    panic_hook.restore();
    let result = match result {
        Ok(result) => result,
        Err(payload) => panic::resume_unwind(payload),
    };
    presentation_cleanup_result.context("failed to clear the TUI viewport before exit")?;
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
    alternate_screen_active: bool,
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
            alternate_screen_active: false,
            keyboard_enhancement_enabled: false,
            bracketed_paste_enabled: false,
            mouse_capture_active: false,
            focus_change_active: false,
        }
    }

    fn restore(&mut self) -> io::Result<()> {
        let mut stdout = io::stdout();
        let mut first_error = None;
        if self.mouse_capture_active {
            remember_cleanup_error(execute!(stdout, DisableMouseCapture), &mut first_error);
            self.mouse_capture_active = false;
        }
        if self.focus_change_active {
            remember_cleanup_error(execute!(stdout, DisableFocusChange), &mut first_error);
            self.focus_change_active = false;
        }
        if self.bracketed_paste_enabled {
            remember_cleanup_error(execute!(stdout, DisableBracketedPaste), &mut first_error);
            self.bracketed_paste_enabled = false;
        }
        if self.keyboard_enhancement_enabled {
            remember_cleanup_error(
                execute!(stdout, PopKeyboardEnhancementFlags),
                &mut first_error,
            );
            self.keyboard_enhancement_enabled = false;
        }
        if self.alternate_screen_active {
            remember_cleanup_error(leave_terminal_presentation(&mut stdout), &mut first_error);
            self.alternate_screen_active = false;
        }
        if self.raw_mode_enabled {
            remember_cleanup_error(disable_raw_mode(), &mut first_error);
            self.raw_mode_enabled = false;
        }
        remember_cleanup_error(execute!(stdout, Show), &mut first_error);
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

type TuiPanicHook = dyn Fn(&panic::PanicHookInfo<'_>) + Send + Sync + 'static;

struct TuiPanicHookGuard {
    previous: Option<Arc<TuiPanicHook>>,
}

impl TuiPanicHookGuard {
    #[cfg(not(test))]
    fn install() -> (Self, tokio::sync::mpsc::UnboundedReceiver<String>) {
        Self::install_with_restore(restore_terminal_escape_state)
    }

    fn install_with_restore(
        restore_terminal: impl Fn() + Send + Sync + 'static,
    ) -> (Self, tokio::sync::mpsc::UnboundedReceiver<String>) {
        let previous = Arc::<TuiPanicHook>::from(panic::take_hook());
        let hook_previous = Arc::clone(&previous);
        let owner_thread = std::thread::current().id();
        let (background_panic_tx, background_panic_rx) = tokio::sync::mpsc::unbounded_channel();
        panic::set_hook(Box::new(move |info| {
            if std::thread::current().id() == owner_thread {
                restore_terminal();
                hook_previous(info);
            } else {
                // A Tokio or worker-thread panic must not tear down the terminal owned by the
                // launcher thread. Route it back into the application loop, which will stop the
                // worker and restore the terminal exactly once before surfacing the error.
                let _ = background_panic_tx.send(format_background_panic(info));
            }
        }));
        (
            Self {
                previous: Some(previous),
            },
            background_panic_rx,
        )
    }

    fn restore(mut self) {
        if let Some(previous) = self.previous.take() {
            panic::set_hook(Box::new(move |info| previous(info)));
        }
    }
}

fn format_background_panic(info: &panic::PanicHookInfo<'_>) -> String {
    let thread = std::thread::current();
    let thread_name = thread.name().unwrap_or("unnamed");
    let payload = info
        .payload()
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload");
    let payload = payload.chars().take(512).collect::<String>();
    if let Some(location) = info.location() {
        format!(
            "background thread `{thread_name}` panicked at {}:{}:{}: {payload}",
            location.file(),
            location.line(),
            location.column()
        )
    } else {
        format!("background thread `{thread_name}` panicked: {payload}")
    }
}

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
    let _ = execute!(stdout, DisableMouseCapture);
    let _ = execute!(stdout, DisableFocusChange);
    let _ = execute!(stdout, DisableBracketedPaste);
    let _ = execute!(stdout, PopKeyboardEnhancementFlags);
    let _ = leave_terminal_presentation(&mut stdout);
    let _ = disable_raw_mode();
    let _ = execute!(stdout, Show);
}

fn finalize_terminal_presentation<B: Backend>(terminal: &mut Terminal<B>) -> Result<(), B::Error> {
    let backend = terminal.backend_mut();
    let _ = backend.hide_cursor();
    // `Terminal::clear()` preserves the cursor by calling `Backend::get_cursor_position()`. The
    // Crossterm implementation answers that call with a CPR request, which can race EventStream or
    // time out once the application loop has stopped. Exit cleanup must be write-only.
    backend.clear()?;
    backend.set_cursor_position(Position::ORIGIN)?;
    let _phase_timing = crate::phase_timing::PhaseTimer::new("terminal_flush");
    backend.flush()
}

fn enter_terminal_presentation<W: std::io::Write>(writer: &mut W) -> std::io::Result<()> {
    // Some terminals preserve the previous alternate-screen buffer or expose primary-screen
    // history while an alternate-screen application is active. Ratatui starts with a logically
    // blank back buffer, so it will not overwrite those physically stale blank cells on its first
    // diff. Establish a known empty presentation with write-only commands before the first frame.
    execute!(
        writer,
        EnterAlternateScreen,
        Clear(ClearType::Purge),
        Clear(ClearType::All),
        MoveTo(0, 0)
    )
}

fn leave_terminal_presentation<W: std::io::Write>(writer: &mut W) -> std::io::Result<()> {
    execute!(writer, LeaveAlternateScreen)
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
fn terminal_fullscreen(stdout: io::Stdout) -> Result<TuiTerminal> {
    Terminal::new(CrosstermBackend::new(stdout))
        .context("failed to initialize the full-screen terminal")
}

#[cfg(not(test))]
async fn run_app(
    terminal: &mut TuiTerminal,
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    mouse_capture_active: &mut bool,
    focus_change_active: &mut bool,
    background_panics: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
) -> Result<()> {
    // From this point onward the event stream is the sole reader of terminal input. Full-screen
    // rendering never queries the cursor and never writes transcript rows into native scrollback.
    let mut terminal_events = EventStream::new();
    let mut needs_render = true;
    let mut last_spinner_tick = live_spinner_tick();
    let mut presentation = PresentationSession::new();
    let mut host_effects = SystemHostEffects;
    let mut attention =
        AttentionController::from_current_process(app.terminal_notification_config());

    loop {
        attention.update_config(app.terminal_notification_config());
        let mut dirty = needs_render;
        dirty |= drain_worker_messages_with_attention(app, worker, &mut attention)?;
        dirty |= restart_worker_after_session_transition(app, worker, spawn_worker)?;
        if dirty {
            dirty |= refresh_application_projection(app, worker).await;
        }
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

        terminal.autoresize()?;
        let frame_area = terminal.get_frame().area();
        dirty |= app.set_terminal_size(frame_area.width, frame_area.height);

        let spinner_tick = live_spinner_tick();
        if app.runtime.is_busy && spinner_tick != last_spinner_tick {
            dirty = true;
        }

        if dirty {
            render_timed_frame(terminal, app, &mut presentation)?;
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
            BackgroundPanic(String),
            Deadline,
        }
        let wake = {
            let terminal_event = terminal_events.next();
            let worker_message = next_worker_message(worker);
            match next_wake_deadline(app) {
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
                    report = background_panics.recv() => WakeEvent::BackgroundPanic(
                        report.unwrap_or_else(|| "background panic channel closed".to_owned()),
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
                    report = background_panics.recv() => WakeEvent::BackgroundPanic(
                        report.unwrap_or_else(|| "background panic channel closed".to_owned()),
                    ),
                },
            }
        };
        match wake {
            WakeEvent::Terminal(event) => {
                let mut next_event = Some(InputEvent::from(event?));
                let mut batch_damage = Damage::NONE;
                for processed_events in 0..EVENT_BATCH_LIMIT {
                    let Some(crossterm_event) = next_event.take() else {
                        break;
                    };
                    let (break_batch, event_damage) = process_terminal_event(
                        terminal,
                        app,
                        worker,
                        &presentation,
                        crossterm_event,
                        &mut attention,
                        spawn_worker,
                        &mut host_effects,
                    )?;
                    batch_damage = batch_damage.union(event_damage);
                    if break_batch || processed_events + 1 >= EVENT_BATCH_LIMIT {
                        break;
                    }
                    next_event = event::poll(Duration::ZERO)?
                        .then(event::read)
                        .transpose()?
                        .map(InputEvent::from);
                }
                needs_render |= !batch_damage.is_empty();
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
            WakeEvent::BackgroundPanic(report) => anyhow::bail!(report),
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
    presentation: &PresentationSession,
    input_event: InputEvent,
    attention: &mut AttentionController,
    spawn_worker: F,
    host_effects: &mut impl HostEffects,
) -> Result<(bool, Damage)>
where
    F: FnMut(RootConfig, &AppState) -> Result<WorkerRuntime>,
{
    match input_event {
        InputEvent::Resize { .. } => {
            terminal.autoresize()?;
            Ok((true, Damage::TERMINAL))
        }
        InputEvent::Mouse(mouse) => {
            let Some(committed) = presentation.current() else {
                // There is no safe coordinate authority before the first successful frame or
                // while the terminal epoch is poisoned. The owner loop will either present a
                // fresh frame or terminate after a present fault.
                return Ok((false, Damage::NONE));
            };
            let outcome = app.handle_committed_mouse_event(mouse.into(), committed)?;
            let damage =
                apply_mouse_outcome_with_host(app, worker, outcome, spawn_worker, host_effects)?;
            Ok((false, damage))
        }
        InputEvent::Paste(text) => {
            app.handle_paste_text(&text);
            Ok((false, Damage::INPUT))
        }
        InputEvent::Focus(FocusChange::Gained) => {
            attention.observe_focus(true);
            Ok((false, Damage::NONE))
        }
        InputEvent::Focus(FocusChange::Lost) => {
            attention.observe_focus(false);
            Ok((false, Damage::NONE))
        }
        InputEvent::Key(key) if key.kind == InputKeyEventKind::Press => {
            if matches!(key.code, InputKeyCode::Char('v') | InputKeyCode::Char('V'))
                && key.modifiers == Modifiers::CONTROL
                && app.can_accept_image_attachment_input()
            {
                match host_effects.read_clipboard_image_png() {
                    Ok(Some(encoded_png)) => {
                        app.handle_clipboard_image(encoded_png);
                        return Ok((false, Damage::HOST_EFFECT));
                    }
                    Ok(None) => {}
                    Err(_) => {
                        app.report_clipboard_image_failure();
                        return Ok((false, Damage::HOST_EFFECT));
                    }
                }
            }
            let may_change_state = key.may_change_state();
            let action = app.handle_key_event(key.to_crossterm())?;
            let damage = apply_key_action_with_host(
                app,
                worker,
                action,
                if may_change_state {
                    Damage::INPUT
                } else {
                    Damage::NONE
                },
                spawn_worker,
                host_effects,
            )?;
            Ok((false, damage))
        }
        InputEvent::Key(_) => Ok((false, Damage::NONE)),
    }
}

#[cfg(test)]
fn mouse_layout_snapshot(frame_area: Rect, terminal_size: Rect, app: &AppState) -> LayoutSnapshot {
    let screen = if frame_area.width == 0 || frame_area.height == 0 {
        Rect::new(0, 0, terminal_size.width, terminal_size.height)
    } else {
        frame_area
    };
    LayoutSnapshot::from_app(screen, app)
}

fn render_timed_frame<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut AppState,
    presentation: &mut PresentationSession,
) -> Result<()> {
    let _phase_timing = crate::phase_timing::PhaseTimer::new("terminal_present");
    let generation = presentation
        .begin_prepare()
        .map_err(|error| anyhow::anyhow!(error))?;
    let attempt = presentation
        .begin_present(generation)
        .map_err(|error| anyhow::anyhow!(error))?;
    let mut prepared = None;
    let mut egress_rendered = false;
    app.begin_egress_disclosure_frame();
    let draw_result = terminal.draw(|frame| {
        // Layout, cell output, and the interaction facts are captured by this one render
        // transaction. Input never reconstructs this layout from the mutable AppState.
        let surface = build_surface_model(
            frame.area(),
            app,
            crate::surface::SurfaceState {
                frame_generation: generation.value(),
                terminal_epoch: presentation.terminal_epoch().value(),
            },
        );
        egress_rendered = surface.egress_disclosure.is_some()
            && surface.layout.egress_disclosure.is_some()
            && !surface.user_input_open
            && !surface.plan_workbench_open;
        let layout = std::sync::Arc::new(surface.layout.clone());
        ui::render_surface(frame, &surface);
        prepared = Some((frame.area(), layout, frame.buffer_mut().clone()));
    });
    if let Err(error) = draw_result {
        presentation
            .fail_after_io(generation, attempt, error.to_string())
            .map_err(|state_error| anyhow::anyhow!(state_error))?;
        return Err(anyhow::anyhow!("terminal present failed: {error}"));
    }
    if egress_rendered {
        app.mark_egress_disclosure_rendered();
    }
    let Some((area, layout, surface)) = prepared else {
        presentation
            .fail_after_io(generation, attempt, "terminal draw callback did not run")
            .map_err(|state_error| anyhow::anyhow!(state_error))?;
        return Err(anyhow::anyhow!(
            "terminal present failed: draw callback did not run"
        ));
    };
    presentation
        .finish_draw(generation, attempt, area, surface, layout)
        .map_err(|error| anyhow::anyhow!(error))?;
    Ok(())
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
    let boot_result = match load_result {
        Ok(_root_config) => {
            sigil_runtime::r71_authority_composition::boot_current_schema(&config_path, &cwd)
                .map_err(anyhow::Error::new)
        }
        Err(error) => Err(error),
    };
    build_initial_app_with_session(
        cwd,
        config_path,
        boot_result,
        InitialSessionTarget::Fresh,
        spawn_worker_fn,
    )
}

fn build_initial_app_with_session<F>(
    cwd: PathBuf,
    config_path: PathBuf,
    boot_result: Result<sigil_runtime::r71_authority_composition::RuntimeCurrentBootTransactionV1>,
    initial_session: InitialSessionTarget<'_>,
    mut spawn_worker_fn: F,
) -> Result<(AppState, Option<WorkerRuntime>)>
where
    F: FnMut(RootConfig, &AppState) -> Result<WorkerRuntime>,
{
    let mut worker = None;
    let app = match boot_result {
        Ok(transaction) => {
            // Runtime owns the indivisible current-schema boot transaction. The TUI consumes its
            // frozen config/path/composition values and performs no independent authority load.
            let (root_config, workspace_root, paths, boot_cutover, composition, _registration) =
                transaction.into_published_parts();
            let mut app = AppState::from_root_config(&config_path, &root_config);
            app.set_frozen_boot_paths(workspace_root, paths);
            app.set_boot_cutover(std::sync::Arc::new(boot_cutover));
            app.set_authority_composition(std::sync::Arc::new(composition));
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

#[cfg(not(test))]
#[doc(hidden)]
pub fn install_current_boot_transaction(
    app: &mut AppState,
    config_path: &Path,
    session_route: Option<sigil_kernel::ResolvedModelRoute>,
    launch_cwd: &Path,
) -> Result<RootConfig> {
    // Authority composition always loads and validates the persisted configuration itself. The
    // optional route is a narrow session overlay used only to derive the worker configuration;
    // it can never select workspace/storage/execution authority roots.
    let transaction =
        sigil_runtime::r71_authority_composition::boot_current_schema(config_path, launch_cwd)
            .map_err(anyhow::Error::new)?;
    let persisted_config = transaction.config().clone();
    let session_config = session_route
        .as_ref()
        .map(|route| app.runtime_config_for_session_route(persisted_config.clone(), route))
        .transpose()?
        .unwrap_or_else(|| persisted_config.clone());
    let (persisted_config, workspace_root, paths, boot_cutover, composition, _registration) =
        transaction.into_published_parts();
    app.set_frozen_boot_paths(workspace_root, paths);
    app.set_boot_cutover(std::sync::Arc::new(boot_cutover));
    app.set_authority_composition(std::sync::Arc::new(composition));
    app.apply_persisted_config_snapshot(&persisted_config);
    app.apply_session_runtime_config(&session_config);
    Ok(session_config)
}

#[cfg(test)]
fn install_current_boot_transaction(
    app: &mut AppState,
    _config_path: &Path,
    session_route: Option<sigil_kernel::ResolvedModelRoute>,
    _launch_cwd: &Path,
) -> Result<RootConfig> {
    // Unit action tests inject a worker factory and intentionally exercise only action ordering;
    // the shipping launcher uses the production implementation above.
    let persisted_config = app
        .persisted_config_snapshot()
        .cloned()
        .context("test boot requires persisted config")?;
    let session_config = session_route
        .as_ref()
        .map(|route| app.runtime_config_for_session_route(persisted_config.clone(), route))
        .transpose()?
        .unwrap_or_else(|| persisted_config.clone());
    app.apply_session_runtime_config(&session_config);
    Ok(session_config)
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

#[cfg(test)]
fn process_app_action_with_spawner<F>(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    action: AppAction,
    spawn_worker_fn: F,
) -> Result<()>
where
    F: FnMut(RootConfig, &AppState) -> Result<WorkerRuntime>,
{
    let mut host_effects = TestHostEffects;
    process_app_action_with_spawner_and_host(
        app,
        worker,
        action,
        spawn_worker_fn,
        &mut host_effects,
    )
}

fn process_app_action_with_spawner_and_host<F, H>(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    action: AppAction,
    mut spawn_worker_fn: F,
    host_effects: &mut H,
) -> Result<()>
where
    F: FnMut(RootConfig, &AppState) -> Result<WorkerRuntime>,
    H: HostEffects,
{
    match action {
        AppAction::SetupCompleted {
            config_path,
            root_config,
        } => {
            let support_build_info = app.support_build_info().clone();
            let update_build_info = app.update_build_info().clone();
            let root_config = *root_config;
            *app = AppState::from_root_config(&config_path, &root_config);
            app.set_support_build_info(support_build_info);
            app.set_update_build_info(update_build_info);
            let root_config = match install_current_boot_transaction(
                app,
                &config_path,
                app.current_session_route(),
                &std::env::current_dir()?,
            ) {
                Ok(root_config) => root_config,
                Err(error) => {
                    report_worker_unavailable(
                        app,
                        &format!("setup completed but authority boot is unavailable: {error:#}"),
                    )?;
                    return Ok(());
                }
            };
            if let Err(error) =
                app.ensure_current_workspace_trust_decision("trusted by user during quick setup")
            {
                apply_worker_startup_recovery(app, &error, &app.session_log_path.clone())?;
                return Ok(());
            }
            match spawn_worker_fn(root_config, app) {
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
            let Some(root_config) = app.session_runtime_config_snapshot().cloned() else {
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
        AppAction::PersistConfiguration { request } => {
            let persist_action = AppAction::PersistConfiguration {
                request: std::sync::Arc::clone(&request),
            };
            let receipt = match try_execute_application_action(app, worker, &persist_action) {
                Ok(Some(receipt)) => receipt,
                Ok(None) => {
                    report_worker_unavailable(
                        app,
                        "configuration save requires the application port",
                    )?;
                    return Ok(());
                }
                Err(error) => {
                    report_worker_unavailable(
                        app,
                        &format!("configuration save was not admitted: {error}"),
                    )?;
                    return Ok(());
                }
            };
            let settled = matches!(
                receipt,
                sigil_application::ApplicationCommandReceipt::Settled(_)
                    | sigil_application::ApplicationCommandReceipt::Replayed(_)
            );
            report_application_receipt(app, &receipt)?;
            if settled {
                return process_app_action_with_spawner_and_host(
                    app,
                    worker,
                    AppAction::ConfigSaved {
                        root_config: Box::new(request.next_base.clone()),
                    },
                    spawn_worker_fn,
                    host_effects,
                );
            }
        }
        AppAction::ConfigSaved { .. } | AppAction::RuntimeConfigUpdated { .. } => {
            let Some(session_route) = app.current_session_route() else {
                return Ok(());
            };
            let config_path = app.config_path.clone();
            let launch_cwd = std::env::current_dir()?;
            shutdown_and_join_worker(worker);
            #[cfg(not(test))]
            app.clear_boot_authority();
            let runtime_config = match install_current_boot_transaction(
                app,
                &config_path,
                Some(session_route),
                &launch_cwd,
            ) {
                Ok(config) => config,
                Err(error) => {
                    report_worker_unavailable(
                        app,
                        &format!("configuration saved but authority reboot failed: {error:#}"),
                    )?;
                    return Ok(());
                }
            };
            match spawn_worker_fn(runtime_config, app) {
                Ok(runtime) => *worker = Some(runtime),
                Err(error) => report_worker_unavailable(
                    app,
                    &format!("configuration saved; agent runtime remains unavailable: {error:#}"),
                )?,
            }
        }
        AppAction::SessionRuntimeRouteUpdated { route } => {
            let persisted_config = app
                .persisted_config_snapshot()
                .cloned()
                .context("session route update requires the persisted runtime config")?;
            let runtime_config = app.runtime_config_for_session_route(persisted_config, &route)?;
            app.apply_session_runtime_config(&runtime_config);
            shutdown_and_join_worker(worker);
            match spawn_worker_fn(runtime_config, app) {
                Ok(runtime) => *worker = Some(runtime),
                Err(error) => report_worker_unavailable(
                    app,
                    &format!("model route changed; agent runtime remains unavailable: {error:#}"),
                )?,
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
            match try_execute_application_action(
                app,
                worker,
                &AppAction::StartNewSession {
                    session_log_path: session_log_path.clone(),
                },
            ) {
                Ok(Some(receipt)) => {
                    report_application_receipt(app, &receipt)?;
                    return Ok(());
                }
                Ok(None) => {}
                Err(error) => {
                    report_worker_unavailable(
                        app,
                        &format!("application session creation was not admitted: {error}"),
                    )?;
                    return Ok(());
                }
            }
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
                .session_runtime_config_snapshot()
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
            match try_execute_application_action(
                app,
                worker,
                &AppAction::SwitchSession {
                    session_log_path: session_log_path.clone(),
                },
            ) {
                Ok(Some(receipt)) => {
                    report_application_receipt(app, &receipt)?;
                    return Ok(());
                }
                Ok(None) => {}
                Err(error) => {
                    report_worker_unavailable(
                        app,
                        &format!("application session switch was not admitted: {error}"),
                    )?;
                    return Ok(());
                }
            }
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
                .session_runtime_config_snapshot()
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
            process_host_request(
                app,
                HostRequest::CopyText {
                    text,
                    secret: false,
                },
                host_effects,
            )?;
        }
        AppAction::CopySecretToClipboard { text } => {
            process_host_request(
                app,
                HostRequest::CopyText {
                    text: text.expose_secret().to_owned(),
                    secret: true,
                },
                host_effects,
            )?;
        }
        AppAction::OpenExternalUrl { url } => {
            process_host_request(
                app,
                HostRequest::OpenExternalUrl { url, secret: false },
                host_effects,
            )?;
        }
        AppAction::OpenSecretExternalUrl { url } => {
            process_host_request(
                app,
                HostRequest::OpenExternalUrl {
                    url: url.expose_secret().to_owned(),
                    secret: true,
                },
                host_effects,
            )?;
        }
        AppAction::RevealFile { path } => {
            process_host_request(app, HostRequest::RevealFile(path), host_effects)?;
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
        action => match try_execute_application_action(app, worker, &action) {
            Ok(Some(receipt)) => report_application_receipt(app, &receipt)?,
            Ok(None) => {
                let command = app.into_worker_command(action);
                send_worker_command_with_restart(app, worker, command, &mut spawn_worker_fn)?;
            }
            Err(error) => {
                report_worker_unavailable(
                    app,
                    &format!("application command was not admitted: {error}"),
                )?;
            }
        },
    }
    flush_pending_worker_commands(app, worker)?;
    Ok(())
}

fn try_execute_application_action(
    app: &AppState,
    worker: &Option<WorkerRuntime>,
    action: &AppAction,
) -> Result<Option<sigil_application::ApplicationCommandReceipt>> {
    #[cfg(not(test))]
    {
        worker
            .as_ref()
            .map(|runtime| {
                let attachment_recovery_binding = match action {
                    AppAction::SwitchSession { session_log_path } => {
                        app.pending_session_attachment_recovery_binding_for(session_log_path)
                    }
                    _ => None,
                };
                runtime.application.try_execute_action(
                    action,
                    app.active_conversation_queue_target().as_ref(),
                    attachment_recovery_binding,
                )
            })
            .transpose()?
            .flatten()
            .map_or(Ok(None), |receipt| Ok(Some(receipt)))
    }
    #[cfg(test)]
    {
        let _ = (app, worker, action);
        Ok(None)
    }
}

#[cfg(not(test))]
async fn refresh_application_projection(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
) -> bool {
    let Some(application) = worker.as_ref().map(|runtime| &runtime.application) else {
        return false;
    };
    match application.refresh().await {
        Ok(projection) => app.apply_application_projection(&projection),
        Err(error) => {
            tracing::debug!(%error, "application projection refresh unavailable");
            false
        }
    }
}

fn report_application_receipt(
    app: &mut AppState,
    receipt: &sigil_application::ApplicationCommandReceipt,
) -> Result<()> {
    let notice = match receipt {
        sigil_application::ApplicationCommandReceipt::Settled(_) => return Ok(()),
        sigil_application::ApplicationCommandReceipt::Replayed(_) => "application command replayed",
        sigil_application::ApplicationCommandReceipt::ReplayedUncertain(_) => {
            "application command replayed; waiting for durable outcome"
        }
        sigil_application::ApplicationCommandReceipt::Rejected(rejection) => {
            return app.handle_worker_message(WorkerMessage::Notice(format!(
                "application command rejected: {}",
                rejection.reason
            )));
        }
        sigil_application::ApplicationCommandReceipt::PayloadConflict(_) => {
            "application command payload conflicts with its durable reservation"
        }
        sigil_application::ApplicationCommandReceipt::InFlight(_) => {
            "application command is already in flight"
        }
        sigil_application::ApplicationCommandReceipt::Uncertain(_) => {
            "application command dispatched; waiting for durable outcome"
        }
    };
    app.handle_worker_message(WorkerMessage::Notice(notice.to_owned()))
}

fn process_host_request<H: HostEffects>(
    app: &mut AppState,
    request: HostRequest,
    host_effects: &mut H,
) -> Result<()> {
    match request {
        HostRequest::CopyText {
            text,
            secret: _secret,
        } => match host_effects.copy_text(&text, app.terminal_osc52_clipboard_enabled()) {
            crate::clipboard::ClipboardCopyOutcome::Copied => {
                app.record_clipboard_copy_success(&text)
            }
            crate::clipboard::ClipboardCopyOutcome::Unavailable(reason) => {
                app.record_clipboard_copy_unavailable(&reason)
            }
        },
        HostRequest::OpenExternalUrl { url, secret } => {
            let target = ExternalLaunchTarget::Url(&url);
            let success_notice = if secret {
                "opening authorization page"
            } else {
                "opening bug report form"
            };
            let failure_notice = if secret {
                "open authorization page"
            } else {
                "open bug report form"
            };
            match host_effects.launch_external(target) {
                Ok(()) => app.record_feedback_external_action_success(success_notice),
                Err(error) => app.record_feedback_external_action_failure(failure_notice, &error),
            }
        }
        HostRequest::RevealFile(path) => {
            match host_effects.launch_external(ExternalLaunchTarget::RevealFile(&path)) {
                Ok(()) => app.record_feedback_external_action_success("revealing feedback report"),
                Err(error) => {
                    app.record_feedback_external_action_failure("reveal feedback report", &error)
                }
            }
        }
    }
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
    let Some(root_config) = app.session_runtime_config_snapshot().cloned() else {
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

    let Some(root_config) = app.session_runtime_config_snapshot().cloned() else {
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

#[cfg(test)]
fn apply_mouse_outcome<F>(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    outcome: AppMouseOutcome,
    mut spawn_worker_fn: F,
) -> Result<bool>
where
    F: FnMut(RootConfig, &AppState) -> Result<WorkerRuntime>,
{
    let mut host_effects = TestHostEffects;
    let damage = apply_mouse_outcome_with_host(
        app,
        worker,
        outcome,
        &mut spawn_worker_fn,
        &mut host_effects,
    )?;
    Ok(!damage.is_empty())
}

fn apply_mouse_outcome_with_host<F, H>(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    outcome: AppMouseOutcome,
    mut spawn_worker_fn: F,
    host_effects: &mut H,
) -> Result<Damage>
where
    F: FnMut(RootConfig, &AppState) -> Result<WorkerRuntime>,
    H: HostEffects,
{
    apply_event_effect(
        app,
        worker,
        outcome.into_event_effect(),
        &mut spawn_worker_fn,
        host_effects,
    )
}

#[cfg(test)]
fn apply_key_action<F>(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    action: Option<AppAction>,
    mut spawn_worker_fn: F,
) -> Result<bool>
where
    F: FnMut(RootConfig, &AppState) -> Result<WorkerRuntime>,
{
    let mut host_effects = TestHostEffects;
    let damage = apply_key_action_with_host(
        app,
        worker,
        action,
        Damage::FULL,
        &mut spawn_worker_fn,
        &mut host_effects,
    )?;
    Ok(!damage.is_empty())
}

fn apply_key_action_with_host<F, H>(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    action: Option<AppAction>,
    no_action_damage: Damage,
    mut spawn_worker_fn: F,
    host_effects: &mut H,
) -> Result<Damage>
where
    F: FnMut(RootConfig, &AppState) -> Result<WorkerRuntime>,
    H: HostEffects,
{
    let effect = action.map_or(
        EventEffect::LocalUpdate(no_action_damage),
        AppAction::into_event_effect,
    );
    apply_event_effect(app, worker, effect, &mut spawn_worker_fn, host_effects)
}

fn apply_event_effect<F, H>(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    effect: EventEffect,
    spawn_worker_fn: &mut F,
    host_effects: &mut H,
) -> Result<Damage>
where
    F: FnMut(RootConfig, &AppState) -> Result<WorkerRuntime>,
    H: HostEffects,
{
    match effect {
        EventEffect::Ignored => Ok(Damage::NONE),
        EventEffect::LocalUpdate(damage) => Ok(damage),
        EventEffect::OpaqueAction(action) => {
            process_app_action_with_spawner_and_host(
                app,
                worker,
                action,
                spawn_worker_fn,
                host_effects,
            )?;
            Ok(Damage::ASYNC)
        }
        EventEffect::HostRequest(request) => {
            process_host_request(app, request, host_effects)?;
            Ok(Damage::HOST_EFFECT)
        }
    }
}

fn next_mouse_capture_action(active: bool, desired: bool) -> Option<bool> {
    if active == desired {
        return None;
    }
    Some(desired)
}

fn next_wake_deadline(app: &AppState) -> Option<Duration> {
    if app.runtime.is_busy {
        Some(Duration::from_millis(SPINNER_FRAME_MILLIS as u64))
    } else if app.has_pending_background_tasks() {
        Some(BACKGROUND_TASK_WAKE_INTERVAL)
    } else {
        None
    }
}

fn render_tui_exit_resume_hint(app: &AppState, explicit_config: Option<&Path>) -> String {
    if app.is_setup_mode()
        || app.is_workspace_trust_gate_mode()
        || !app.current_session_has_resumable_activity()
    {
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
    #[cfg(not(test))]
    application: application_bridge::TuiApplicationSession,
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
        app.authority_composition().cloned(),
        app.boot_cutover().cloned(),
        app.worker_session_attachment(),
    )?;
    let application = match application_bridge::build_for_worker(
        app,
        spawned.command_tx.clone(),
        app.runtime.reasoning_effort.clone(),
    ) {
        Ok(application) => application,
        Err(error) => {
            let _ = spawned.command_tx.send(WorkerCommand::Shutdown);
            let _ = spawned.join_handle.join();
            return Err(error.context("failed to attach TUI application port"));
        }
    };
    Ok(WorkerRuntime {
        worker_tx: spawned.command_tx,
        application,
        worker_rx: WorkerMessageInbox::forward_from(spawned.message_rx)?,
        join_handle: Some(spawned.join_handle),
        ready: false,
    })
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/main_tests.rs"]
mod tests;
