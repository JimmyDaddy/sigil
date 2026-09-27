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
#[cfg(test)]
use sigil_kernel::JsonlSessionStore;
use sigil_kernel::RootConfig;
#[cfg(not(test))]
use sigil_kernel::TerminalKeyboardEnhancement;
#[cfg(not(test))]
use sigil_kernel::preferred_config_path;
#[cfg(not(test))]
use sigil_runtime::support::SupportBuildInfo;
#[cfg(not(test))]
use sigil_updater::BuildMetadata;

use crate::application_bridge;
#[cfg(not(test))]
use crate::host_effects::SystemHostEffects;
#[cfg(test)]
use crate::host_effects::{ExternalLaunchPlatform, TestHostEffects, external_launch_plan};
#[cfg(not(test))]
use crate::input_event::{FocusChange, InputEvent, InputKeyCode, InputKeyEventKind, Modifiers};
use crate::ui;
pub(crate) mod control_log_recovery;
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

pub(crate) mod shutdown;
use shutdown::{ShutdownPass, ShutdownPoll, poll_owned_thread};

mod runtime_transition;
pub(crate) use runtime_transition::{RuntimeMaintenanceOwner, RuntimeTransitionOwner};

#[path = "launcher_projection_retry.rs"]
mod projection_retry;
#[cfg(not(test))]
use projection_retry::{
    ProjectionFailure, ProjectionRetry, projection_wake_deadline, reconcile_projection_owner,
    should_replace_pending_ack,
};

const BACKGROUND_TASK_WAKE_INTERVAL: Duration = Duration::from_millis(250);
const WORKER_MESSAGE_BATCH_LIMIT: usize = 64;
const WORKER_MESSAGE_BATCH_BUDGET: Duration = Duration::from_millis(4);
const WORKER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(not(test))]
const EVENT_BATCH_LIMIT: usize = 64;
const SPINNER_FRAME_MILLIS: u128 = 120;

#[cfg(not(test))]
type TuiTerminal = Terminal<CrosstermBackend<io::Stdout>>;

#[cfg(not(test))]
#[allow(dead_code)]
pub fn run_tui(config: Option<PathBuf>) -> Result<()> {
    run_tui_with_build_info(config, SupportBuildInfo::unknown())
}

#[cfg(not(test))]
#[allow(dead_code)]
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
#[allow(dead_code)]
pub fn run_tui_resume(config: Option<PathBuf>, session_selector: Option<String>) -> Result<()> {
    run_tui_resume_with_build_info(config, session_selector, SupportBuildInfo::unknown())
}

#[cfg(not(test))]
#[allow(dead_code)]
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
    let boot_result = sigil_runtime::application_host::boot_current_schema(&config_path, &cwd)
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
    let mut shutdown = TuiShutdownState::default();
    let mut owned_event_runtime = None;
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
                        &mut shutdown,
                    ))
                })
            }
            Ok(_) => anyhow::bail!(
                "the synchronous TUI launcher cannot run inside a current-thread Tokio runtime"
            ),
            Err(_) => {
                owned_event_runtime = Some(
                    tokio::runtime::Builder::new_current_thread()
                        .enable_time()
                        .build()
                        .context("failed to build TUI event runtime")?,
                );
                owned_event_runtime
                    .as_ref()
                    .expect("event runtime initialized")
                    .block_on(run_app(
                        &mut terminal,
                        &mut app,
                        &mut worker,
                        &mut mouse_capture_active,
                        &mut focus_change_active,
                        &mut background_panics,
                        &mut shutdown,
                    ))
            }
        },
    ));
    // The panic hook has already restored the primary screen before catch_unwind observes the
    // payload. Clearing through the stale Terminal in that case would erase the panic report from
    // the primary screen. Ordinary Result errors have not run the hook and still need finalizing.
    let presentation_cleanup_result = if result.is_err() {
        Ok(())
    } else {
        finalize_terminal_presentation(&mut terminal)
    };
    cleanup.mouse_capture_active = mouse_capture_active;
    cleanup.focus_change_active = focus_change_active;
    let cleanup_result = cleanup.restore();
    let clean_exit = matches!(&result, Ok(Ok(())))
        && cleanup_result.is_ok()
        && presentation_cleanup_result.is_ok();
    let shutdown_result = shutdown::shutdown_tui_owners(
        &mut app,
        &mut worker,
        &mut shutdown,
        &mut owned_event_runtime,
        clean_exit,
        |notice| eprintln!("{notice}"),
    );
    let background_panic_during_shutdown = background_panics.try_recv().ok();
    let result = match (result, background_panic_during_shutdown) {
        (Ok(Ok(())), Some(report)) => Ok(Err(anyhow::anyhow!(report))),
        (result, _) => result,
    };
    panic_hook.restore();
    let result = match result {
        Ok(result) => result,
        Err(payload) => panic::resume_unwind(payload),
    };
    finish_tui_shutdown(
        result,
        [
            presentation_cleanup_result.context("failed to clear the TUI viewport before exit"),
            cleanup_result.map_err(anyhow::Error::new),
            shutdown_result,
        ],
    )?;
    print!("{}", render_tui_exit_resume_hint(&app, config.as_deref()));
    Ok(())
}

fn finish_tui_shutdown(
    result: Result<()>,
    cleanup_results: impl IntoIterator<Item = Result<()>>,
) -> Result<()> {
    let mut errors = result
        .err()
        .into_iter()
        .chain(cleanup_results.into_iter().filter_map(Result::err));
    let Some(primary) = errors.next() else {
        return Ok(());
    };
    let additional = errors.map(|error| format!("{error:#}")).collect::<Vec<_>>();
    if additional.is_empty() {
        return Err(primary);
    }
    // Cleanup is already complete or accounted for. Keep the original failure first, including
    // a background panic's location, while retaining every failed owner's cleanup diagnostic.
    Err(anyhow::anyhow!(
        "{primary:#}; additional shutdown failures: {}",
        additional.join("; ")
    ))
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
    let payload = format_panic_payload(info.payload());
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

fn format_panic_payload(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
        .chars()
        .take(512)
        .collect()
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
    shutdown: &mut TuiShutdownState,
) -> Result<()> {
    let TuiShutdownState {
        started: shutdown_started,
        projection_observation_owners,
        ..
    } = shutdown;
    // From this point onward the event stream is the sole reader of terminal input. Full-screen
    // rendering never queries the cursor and never writes transcript rows into native scrollback.
    let mut terminal_events = EventStream::new();
    // Keep one `EventStream::next()` future alive across worker/projection wakeups. Crossterm's
    // EventStream has a single background poll task and remembers the first future's waker while
    // that task is blocked in `poll_internal`. Recreating and dropping the future whenever another
    // `select!` branch wins leaves the task holding a stale waker, so later key presses never wake
    // this loop.
    let mut terminal_event = Box::pin(terminal_events.next());
    let mut needs_render = true;
    let mut projection_refresh_requested = true;
    let mut projection_epoch = 1_u64;
    let mut projection_owner = None;
    let mut projection_refresh_retry = ProjectionRetry::new(projection_epoch);
    let mut projection_ack_retry = ProjectionRetry::new(projection_epoch);
    let mut projection_refresh_task: Option<ProjectionRefreshTask> = None;
    let mut projection_ack_task: Option<ProjectionAckTask> = None;
    let mut projection_delivery_task: Option<ProjectionDeliveryTask> = None;
    let mut delivery_requested = false;
    let mut delivery_retry = ProjectionRetry::new(projection_epoch);
    let mut pending_projection_ack: Option<PendingProjectionAck> = None;
    let mut last_spinner_tick = live_spinner_tick();
    let mut presentation = PresentationSession::new();
    let mut host_effects = SystemHostEffects;
    let mut attention =
        AttentionController::from_current_process(app.terminal_notification_config());

    loop {
        release_finished_projection_observations(projection_observation_owners);
        if app.should_quit {
            if let Some(owner) = app.runtime_transition.as_ref() {
                owner.request_exit();
            }
            if let Some(owner) = app.runtime_maintenance.as_ref() {
                owner.request_exit();
            }
            app.cancel_session_auxiliary();
            shutdown_started.get_or_insert_with(Instant::now);
            if let Some(runtime) = worker.as_ref() {
                runtime.worker_tx.reserve_stop(true);
            }
            break;
        }
        attention.update_config(app.terminal_notification_config());
        let mut dirty = needs_render;
        dirty |= runtime_transition::poll(app, worker)?;
        dirty |= runtime_transition::poll_maintenance(app, worker)?;
        let (worker_dirty, worker_projection_refresh) =
            drain_worker_messages_with_attention(app, worker, &mut attention)?;
        dirty |= worker_dirty;
        projection_refresh_requested |= worker_projection_refresh;
        dirty |= app.poll_background_tasks();
        let admission_changed = poll_application_admission(app, worker)?;
        if restart_worker_after_session_transition(app, worker, spawn_worker)? {
            dirty = true;
        }
        dirty |= flush_deferred_application_action(app, worker)?;
        dirty |= control_log_recovery::poll(app)?;
        dirty |= admission_changed;
        projection_refresh_requested |= admission_changed;
        let commands_flushed = flush_pending_worker_commands(app, worker)?;
        dirty |= commands_flushed;
        projection_refresh_requested |= commands_flushed;
        let current_projection_owner = worker
            .as_ref()
            .and_then(|runtime| runtime.application.as_ref())
            .cloned()
            .or_else(|| {
                app.runtime_transition
                    .as_ref()
                    .and_then(|owner| owner.observation_application())
            });
        if reconcile_projection_owner(
            &mut projection_owner,
            current_projection_owner,
            &mut projection_epoch,
            &mut projection_refresh_retry,
            &mut projection_ack_retry,
        ) {
            if let Some(task) = projection_refresh_task.take() {
                task.handle.abort();
            }
            if let Some(task) = projection_ack_task.take() {
                task.handle.abort();
            }
            if let Some(task) = projection_delivery_task.take() {
                task.handle.abort();
            }
            delivery_requested = false;
            delivery_retry = ProjectionRetry::new(projection_epoch);
            pending_projection_ack = None;
            app.attach_session_query_reader(
                projection_owner
                    .as_ref()
                    .map(|application| application.read_handle())
                    .transpose()?,
            );
            projection_refresh_requested = projection_owner.is_some();
            dirty = true;
        }
        if projection_refresh_requested
            && projection_refresh_task.is_none()
            && projection_refresh_retry.can_start(projection_epoch, Instant::now())
        {
            if let Some(application) = worker
                .as_ref()
                .and_then(|runtime| runtime.application.as_ref())
                .cloned()
                .or_else(|| {
                    app.runtime_transition
                        .as_ref()
                        .and_then(|owner| owner.observation_application())
                })
            {
                projection_refresh_requested = false;
                let task_application = Arc::clone(&application);
                let handle = tokio::spawn(async move {
                    refresh_application_projection_task(task_application).await
                });
                retain_projection_observation(
                    projection_observation_owners,
                    Arc::clone(&application),
                    handle.abort_handle(),
                );
                projection_refresh_task = Some(ProjectionRefreshTask {
                    epoch: projection_epoch,
                    application,
                    handle: handle.into(),
                });
            } else {
                projection_refresh_requested = false;
            }
        }
        if projection_delivery_task
            .as_ref()
            .is_some_and(|task| task.handle.is_finished())
        {
            let task = projection_delivery_task
                .take()
                .expect("finished delivery task");
            if task.epoch == projection_epoch {
                match task
                    .handle
                    .await
                    .map_err(|error| ProjectionFailure::Task(error.to_string()))
                    .and_then(|result| result.map_err(ProjectionFailure::Application))
                {
                    Ok(batch) => {
                        delivery_retry.succeeded(task.epoch);
                        for notice in batch.notices {
                            app.handle_worker_message(WorkerMessage::Notice(
                                notice.as_str().to_owned(),
                            ))?;
                            dirty = true;
                        }
                        let event_ids = task.application.take_applied_delivery_event_ids()?;
                        retain_pending_projection_ack(
                            &mut pending_projection_ack,
                            PendingProjectionAck {
                                epoch: task.epoch,
                                application: task.application,
                                frontier: batch.frontier,
                                event_ids,
                            },
                            projection_epoch,
                        );
                        delivery_requested = batch.has_more;
                    }
                    Err(error) => {
                        if let Some(action) =
                            delivery_retry.failed(task.epoch, Instant::now(), &error)
                        {
                            delivery_requested = action.retry;
                            report_projection_failure(
                                app,
                                "Session notification delivery",
                                &error,
                                action.retry,
                            )?;
                            dirty = true;
                        }
                    }
                }
            }
        }
        if delivery_requested
            && projection_delivery_task.is_none()
            && projection_ack_task.is_none()
            && pending_projection_ack.is_none()
            && delivery_retry.can_start(projection_epoch, Instant::now())
            && let Some(application) = projection_owner.as_ref()
            && application.current_projection()?.is_some()
        {
            let task_application = Arc::clone(application);
            let handle = tokio::spawn(async move { task_application.refresh_delivery().await });
            retain_projection_observation(
                projection_observation_owners,
                Arc::clone(application),
                handle.abort_handle(),
            );
            projection_delivery_task = Some(ProjectionDeliveryTask {
                epoch: projection_epoch,
                application: Arc::clone(application),
                handle: handle.into(),
            });
            delivery_requested = false;
        }
        if projection_ack_task.is_none()
            && projection_ack_retry.can_start(projection_epoch, Instant::now())
            && let Some(pending) = pending_projection_ack.take()
        {
            let task_pending = pending.clone();
            let application = Arc::clone(&task_pending.application);
            let handle = tokio::spawn(async move {
                application
                    .acknowledge_public_events(
                        task_pending.event_ids.clone(),
                        &task_pending.frontier,
                    )
                    .await
                    .map(|_| ())
            });
            retain_projection_observation(
                projection_observation_owners,
                Arc::clone(&pending.application),
                handle.abort_handle(),
            );
            projection_ack_task = Some(ProjectionAckTask {
                epoch: pending.epoch,
                handle: handle.into(),
                pending,
            });
        }
        attention.emit_pending_nonfatal(terminal.backend_mut());
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
        if spinner_tick != last_spinner_tick {
            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_millis().min(u128::from(u64::MAX)) as u64)
                .unwrap_or(0);
            dirty |= app.refresh_command_elapsed(now_ms);
        }
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

        enum WakeEvent {
            Terminal(std::io::Result<CrosstermEvent>),
            Worker(Box<WorkerMessage>),
            WorkerClosed,
            BackgroundPanic(String),
            Projection(
                Box<
                    std::result::Result<
                        std::result::Result<
                            ProjectionRefreshOutcome,
                            sigil_application::ApplicationError,
                        >,
                        String,
                    >,
                >,
            ),
            ProjectionAck(
                std::result::Result<
                    std::result::Result<(), sigil_application::ApplicationError>,
                    String,
                >,
            ),
            Deadline,
        }
        let wake = {
            let now = Instant::now();
            let deadline = projection_wake_deadline(
                next_wake_deadline(app)
                    .or_else(|| {
                        (projection_delivery_task.is_some() || delivery_requested)
                            .then_some(BACKGROUND_TASK_WAKE_INTERVAL)
                    })
                    .or_else(|| {
                        worker
                            .as_ref()
                            .filter(|runtime| {
                                runtime
                                    .pending_admission
                                    .as_ref()
                                    .is_some_and(PendingApplicationAdmission::needs_polling)
                                    || runtime
                                        .pending_interactions
                                        .iter()
                                        .any(PendingApplicationAdmission::needs_polling)
                            })
                            .map(|_| BACKGROUND_TASK_WAKE_INTERVAL)
                    }),
                (projection_refresh_requested && projection_refresh_task.is_none())
                    .then(|| projection_refresh_retry.remaining(now))
                    .flatten(),
                (pending_projection_ack.is_some() && projection_ack_task.is_none())
                    .then(|| projection_ack_retry.remaining(now))
                    .flatten(),
            );
            let worker_message = next_worker_message(worker, app.runtime.worker_rebind_required);
            let projection_wake = async {
                if let Some(task) = projection_refresh_task.as_mut() {
                    (&mut task.handle)
                        .await
                        .map_err(|error| format!("projection refresh task failed: {error}"))
                } else {
                    std::future::pending::<
                        std::result::Result<
                            std::result::Result<
                                ProjectionRefreshOutcome,
                                sigil_application::ApplicationError,
                            >,
                            String,
                        >,
                    >()
                    .await
                }
            };
            let projection_ack_wake = async {
                if let Some(task) = projection_ack_task.as_mut() {
                    (&mut task.handle)
                        .await
                        .map_err(|error| format!("projection ACK task failed: {error}"))
                } else {
                    std::future::pending::<
                        std::result::Result<
                            std::result::Result<(), sigil_application::ApplicationError>,
                            String,
                        >,
                    >()
                    .await
                }
            };
            match deadline {
                Some(deadline) => tokio::select! {
                    event = &mut terminal_event => WakeEvent::Terminal(event.unwrap_or_else(|| {
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
                    projection = projection_wake => WakeEvent::Projection(Box::new(projection)),
                    projection_ack = projection_ack_wake => WakeEvent::ProjectionAck(projection_ack),
                    () = tokio::time::sleep(deadline) => WakeEvent::Deadline,
                },
                None => tokio::select! {
                    event = &mut terminal_event => WakeEvent::Terminal(event.unwrap_or_else(|| {
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
                    projection = projection_wake => WakeEvent::Projection(Box::new(projection)),
                    projection_ack = projection_ack_wake => WakeEvent::ProjectionAck(projection_ack),
                },
            }
        };
        match wake {
            WakeEvent::Terminal(event) => {
                // `EventStream::next()` borrows the stream for the duration of the future. The
                // future is safe to replace only after its result has been selected and consumed.
                terminal_event = Box::pin(terminal_events.next());
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
                    projection_refresh_requested |= event_damage.contains(Damage::ASYNC);
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
                projection_refresh_requested |=
                    worker_message_requires_projection_refresh(&message);
                needs_render |=
                    apply_received_worker_message(app, worker, &mut attention, *message)?;
            }
            WakeEvent::WorkerClosed => {
                // The next loop stops scheduling observations while the host joins in background.
                runtime_transition::maintain(app, worker, None)?;
                app.handle_worker_message(WorkerMessage::RunFailed(
                    "agent worker disconnected".to_owned(),
                ))?;
                needs_render = true;
            }
            WakeEvent::Projection(result) => {
                let result = *result;
                let task_owner = projection_refresh_task
                    .take()
                    .map(|task| (task.epoch, task.application));
                let Some((epoch, _application)) = task_owner else {
                    tracing::debug!("discarded TUI projection result without a task owner");
                    continue;
                };
                if epoch != projection_epoch {
                    tracing::debug!(
                        epoch,
                        current_epoch = projection_epoch,
                        "discarded stale TUI application projection"
                    );
                    continue;
                }
                let result = result
                    .map_err(ProjectionFailure::Task)
                    .and_then(|result| result.map_err(ProjectionFailure::Application));
                match result {
                    Ok(outcome) => {
                        if projection_refresh_retry.succeeded(epoch) {
                            report_projection_recovered(app, "Session view update")?;
                            needs_render = true;
                        }
                        let projection_changed =
                            app.apply_application_projection(&outcome.projection);
                        delivery_requested = true;
                        needs_render |= projection_changed;
                    }
                    Err(error) => {
                        if let Some(action) =
                            projection_refresh_retry.failed(epoch, Instant::now(), &error)
                        {
                            // Preserve the exact committed cursor, including on a terminal
                            // refresh with no later worker message to request another attempt.
                            projection_refresh_requested = action.retry;
                            tracing::warn!(
                                ?error,
                                epoch,
                                retry = action.retry,
                                "TUI application projection refresh failed"
                            );
                            if action.notify {
                                report_projection_failure(
                                    app,
                                    "Session view update",
                                    &error,
                                    action.retry,
                                )?;
                                needs_render = true;
                            }
                        }
                    }
                }
            }
            WakeEvent::ProjectionAck(result) => {
                let Some(task) = projection_ack_task.take() else {
                    tracing::debug!("discarded TUI projection ACK result without a task owner");
                    continue;
                };
                let task_epoch = task.epoch;
                if task_epoch != projection_epoch {
                    tracing::debug!(
                        task_epoch,
                        current_epoch = projection_epoch,
                        "discarded stale TUI projection ACK outcome"
                    );
                    continue;
                }
                let result = result
                    .map_err(ProjectionFailure::Task)
                    .and_then(|result| result.map_err(ProjectionFailure::Application));
                match result {
                    Ok(()) => {
                        if projection_ack_retry.succeeded(task_epoch) {
                            report_projection_recovered(app, "Event delivery confirmation")?;
                            needs_render = true;
                        }
                    }
                    Err(error) => {
                        if let Some(action) =
                            projection_ack_retry.failed(task_epoch, Instant::now(), &error)
                        {
                            tracing::warn!(
                                ?error,
                                task_epoch,
                                retry = action.retry,
                                "TUI public event delivery acknowledgement failed"
                            );
                            if action.retry {
                                retain_pending_projection_ack(
                                    &mut pending_projection_ack,
                                    task.pending,
                                    projection_epoch,
                                );
                            }
                            if action.notify {
                                report_projection_failure(
                                    app,
                                    "Event delivery confirmation",
                                    &error,
                                    action.retry,
                                )?;
                                needs_render = true;
                            }
                        }
                    }
                }
            }
            WakeEvent::BackgroundPanic(report) => anyhow::bail!(report),
            WakeEvent::Deadline => {}
        }
    }

    if let Some(task) = projection_refresh_task.take() {
        task.handle.abort();
    }
    if let Some(task) = projection_ack_task.take() {
        task.handle.abort();
    }
    if let Some(task) = projection_delivery_task.take() {
        task.handle.abort();
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
            let break_batch = matches!(
                action,
                Some(AppAction::TrustWorkspace | AppAction::SetupCompleted { .. },)
            );
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
            // Trust and setup completion install the worker and application port synchronously,
            // while the first application projection is committed on the next owner-loop
            // iteration. Stop this input batch so buffered terminal bytes cannot race that
            // initial frontier.
            Ok((break_batch, damage))
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
            sigil_runtime::application_host::boot_current_schema(&config_path, &cwd)
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
    boot_result: Result<sigil_runtime::application_host::RuntimeCurrentBootTransactionV1>,
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
            // Restore the requested target before evaluating trust. A managed session log is
            // the durable source of both the target timeline and its workspace-trust decision;
            // checking the freshly-created bootstrap session first would send `resume` back to
            // the trust gate and then start a new session instead of the requested one.
            restore_initial_session_from_disk(&mut app, &root_config, initial_session)?;
            if app.workspace_is_trusted_from_history() {
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
            // Setup captures the exact source once and classifies it independently from this boot
            // error. Authority/bootstrap failures must not be mistaken for malformed TOML.
            AppState::from_setup_with_recovery(
                config_path,
                cwd,
                startup_error,
                startup_recovery_code_from_error(&error),
            )
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
    expected_config: Option<&RootConfig>,
) -> Result<RootConfig> {
    let transaction = boot_current_transaction(config_path, launch_cwd, expected_config)?;
    install_published_boot_transaction(app, transaction, session_route)
}

#[cfg(not(test))]
fn boot_current_transaction(
    config_path: &Path,
    launch_cwd: &Path,
    expected_config: Option<&RootConfig>,
) -> Result<sigil_runtime::application_host::RuntimeCurrentBootTransactionV1> {
    // Authority composition always loads and validates the persisted configuration itself. The
    // optional expected snapshot is used only to reject a changed setup submission; it can never
    // select workspace/storage/execution authority roots.
    match expected_config {
        Some(expected) => {
            sigil_runtime::application_host::boot_current_schema_with_expected_config(
                config_path,
                launch_cwd,
                expected,
            )
        }
        None => sigil_runtime::application_host::boot_current_schema(config_path, launch_cwd),
    }
    .map_err(anyhow::Error::new)
}

#[cfg(not(test))]
fn install_published_boot_transaction(
    app: &mut AppState,
    transaction: sigil_runtime::application_host::RuntimeCurrentBootTransactionV1,
    session_route: Option<sigil_kernel::ResolvedModelRoute>,
) -> Result<RootConfig> {
    let runtime_config = transaction.runtime_config().clone();
    let session_config = session_route
        .as_ref()
        .map(|route| app.runtime_config_for_session_route(runtime_config.clone(), route))
        .transpose()?
        .unwrap_or(runtime_config);
    let (persisted_config, workspace_root, paths, boot_cutover, composition, _registration) =
        transaction.into_published_parts();
    app.set_frozen_boot_paths(workspace_root, paths);
    app.set_boot_cutover(std::sync::Arc::new(boot_cutover));
    app.set_authority_composition(std::sync::Arc::new(composition));
    app.apply_persisted_config_snapshot(&persisted_config);
    app.apply_session_runtime_config(&session_config);
    Ok(session_config)
}

#[cfg(not(test))]
fn install_setup_boot_transaction(
    app: &mut AppState,
    config_path: &Path,
    expected_config: &RootConfig,
    session_route: Option<sigil_kernel::ResolvedModelRoute>,
    launch_cwd: &Path,
) -> Result<RootConfig> {
    // Keep the setup surface intact until the runtime has completed the authority transaction.
    // A failed journal/bootstrap must therefore return to the existing draft instead of leaving
    // a partially initialized normal AppState behind.
    let transaction = boot_current_transaction(config_path, launch_cwd, Some(expected_config))?;
    let persisted_config = transaction.config().clone();
    control_log_recovery::replace_app_state(
        app,
        AppState::from_root_config(config_path, &persisted_config),
    );
    install_published_boot_transaction(app, transaction, session_route)
}

#[cfg(test)]
fn install_current_boot_transaction(
    app: &mut AppState,
    _config_path: &Path,
    session_route: Option<sigil_kernel::ResolvedModelRoute>,
    _launch_cwd: &Path,
    _expected_config: Option<&RootConfig>,
) -> Result<RootConfig> {
    // Unit action tests inject a worker factory and intentionally exercise only action ordering;
    // the shipping launcher uses the production implementation above.
    let persisted_config = app
        .persisted_config_snapshot()
        .cloned()
        .context("test boot requires persisted config")?;
    let runtime_config = persisted_config.with_effective_composition()?;
    let session_config = session_route
        .as_ref()
        .map(|route| app.runtime_config_for_session_route(runtime_config.clone(), route))
        .transpose()?
        .unwrap_or(runtime_config);
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
    if queue_background_application_action(app, worker, &action)? {
        return Ok(());
    }
    match action {
        AppAction::RecoverControlLog(action) => control_log_recovery::start(app, worker, action)?,
        AppAction::CancelRun => {
            if let Some(runtime) = worker.as_ref() {
                runtime.worker_tx.reserve_stop(false);
                runtime
                    .worker_tx
                    .send(WorkerCommand::CancelRun)
                    .map_err(anyhow::Error::new)?;
            } else {
                app.handle_worker_message(WorkerMessage::Notice(
                    "no active run to stop".to_owned(),
                ))?;
            }
        }
        AppAction::SetupCompleted {
            config_path,
            root_config,
        } => {
            let support_build_info = app.support_build_info().clone();
            let update_build_info = app.update_build_info().clone();
            let setup_notice = app.last_notice().map(str::to_owned);
            let setup_draft = app.setup_state().cloned();
            let root_config = *root_config;
            let session_route = app.current_session_route();
            let launch_cwd = std::env::current_dir()?;
            let boot_result = {
                #[cfg(not(test))]
                {
                    install_setup_boot_transaction(
                        app,
                        &config_path,
                        &root_config,
                        session_route,
                        &launch_cwd,
                    )
                }
                #[cfg(test)]
                {
                    // Unit action tests exercise ordering with the injected test boot stub; the
                    // shipping path above keeps authority boot ahead of normal AppState creation.
                    control_log_recovery::replace_app_state(
                        app,
                        AppState::from_root_config(&config_path, &root_config),
                    );
                    install_current_boot_transaction(
                        app,
                        &config_path,
                        session_route,
                        &launch_cwd,
                        Some(&root_config),
                    )
                }
            };
            let root_config = match boot_result {
                Ok(root_config) => root_config,
                Err(error) => {
                    let startup_error =
                        format!("configuration saved; authority boot is unavailable: {error:#}");
                    if let Some(setup_draft) = setup_draft {
                        let recovery_code = startup_recovery_code_from_error(&error);
                        let mut post_failure_draft = setup_draft.clone();
                        post_failure_draft.startup_error = Some(startup_error.clone());
                        post_failure_draft.startup_recovery_code = recovery_code;
                        return_to_setup_after_boot_failure_with_draft(
                            app,
                            worker,
                            Some(post_failure_draft),
                            startup_error,
                            recovery_code,
                        )?;
                    } else {
                        return_to_setup_after_boot_failure(
                            app,
                            worker,
                            config_path,
                            startup_error,
                            startup_recovery_code_from_error(&error),
                        )?;
                    }
                    return Ok(());
                }
            };
            app.set_support_build_info(support_build_info);
            app.set_update_build_info(update_build_info);
            if let Some(setup_notice) = setup_notice {
                app.set_last_notice(setup_notice);
            }
            if let Err(error) =
                app.ensure_current_workspace_trust_decision("trusted by user during quick setup")
            {
                apply_worker_startup_recovery(app, &error, &app.session_log_path.clone())?;
                return Ok(());
            }
            #[cfg(not(test))]
            return runtime_transition::maintain(app, worker, Some(root_config));
            #[cfg(test)]
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
            #[cfg(not(test))]
            return runtime_transition::maintain(
                app,
                worker,
                app.session_runtime_config_snapshot().cloned(),
            );
            #[cfg(test)]
            {
                shutdown_and_join_worker(worker)?;
                let Some(root_config) = app.session_runtime_config_snapshot().cloned() else {
                    report_worker_unavailable(
                        app,
                        "agent worker stopped; runtime config unavailable",
                    )?;
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
            if settled && let Some(action) = apply_configuration_publication(app, &request)? {
                return process_app_action_with_spawner_and_host(
                    app,
                    worker,
                    action,
                    spawn_worker_fn,
                    host_effects,
                );
            }
        }
        AppAction::ConfigSaved { .. } | AppAction::RuntimeConfigUpdated { .. } => {
            #[cfg(not(test))]
            {
                let route = app
                    .pending_session_route_selection()
                    .map(|(_, route)| route.clone())
                    .or_else(|| app.current_session_route());
                if let Some(route) = route {
                    return runtime_transition::start(app, worker, route);
                }
                return Ok(());
            }
            #[cfg(test)]
            {
                let Some(session_route) = app.current_session_route() else {
                    return Ok(());
                };
                let config_path = app.config_path.clone();
                let launch_cwd = std::env::current_dir()?;
                shutdown_and_join_worker(worker)?;
                let runtime_config = match install_current_boot_transaction(
                    app,
                    &config_path,
                    Some(session_route),
                    &launch_cwd,
                    None,
                ) {
                    Ok(config) => config,
                    Err(error) => {
                        let startup_error =
                            format!("configuration saved but authority reboot failed: {error:#}");
                        let recovery_code = startup_recovery_code_from_error(&error);
                        return_to_setup_after_boot_failure(
                            app,
                            worker,
                            config_path,
                            startup_error,
                            recovery_code,
                        )?;
                        return Ok(());
                    }
                };
                match spawn_worker_fn(runtime_config, app) {
                    Ok(runtime) => *worker = Some(runtime),
                    Err(error) => report_worker_unavailable(
                        app,
                        &format!(
                            "configuration saved; agent runtime remains unavailable: {error:#}"
                        ),
                    )?,
                }
            }
        }
        AppAction::SessionRuntimeRouteUpdated { route } => {
            #[cfg(not(test))]
            return runtime_transition::start(app, worker, route);
            #[cfg(test)]
            {
                let persisted_config = app
                    .persisted_config_snapshot()
                    .cloned()
                    .context("session route update requires the persisted runtime config")?;
                let runtime_config =
                    app.runtime_config_for_session_route(persisted_config, &route)?;
                app.apply_session_runtime_config(&runtime_config);
                shutdown_and_join_worker(worker)?;
                match spawn_worker_fn(runtime_config, app) {
                    Ok(runtime) => *worker = Some(runtime),
                    Err(error) => report_worker_unavailable(
                        app,
                        &format!(
                            "model route changed; agent runtime remains unavailable: {error:#}"
                        ),
                    )?,
                }
            }
        }
        AppAction::SetDefaultModel {
            root_config,
            expected_root_config,
        } => {
            #[cfg(not(test))]
            {
                let request = std::sync::Arc::new(crate::app::ConfigurationSaveRequest {
                    expected: *expected_root_config.clone(),
                    next_base: *root_config.clone(),
                    config_path: app.config_path.clone(),
                    follow_up: crate::app::ConfigurationSaveFollowUp::ApplyPersistedDefaultModel,
                    root_only: true,
                    draft_binding: None,
                    draft: std::sync::Mutex::new(None),
                    published_root_config: std::sync::Mutex::new(None),
                    close_after_save: false,
                });
                let receipt = match try_execute_application_action(
                    app,
                    worker,
                    &AppAction::PersistConfiguration { request },
                ) {
                    Ok(Some(receipt)) => receipt,
                    Ok(None) => {
                        report_worker_unavailable(
                            app,
                            "default model save requires the application port",
                        )?;
                        return Ok(());
                    }
                    Err(error) => {
                        report_worker_unavailable(
                            app,
                            &format!("default model save was not admitted: {error}"),
                        )?;
                        return Ok(());
                    }
                };
                report_application_receipt(app, &receipt)?;
                if matches!(
                    receipt,
                    sigil_application::ApplicationCommandReceipt::Settled(_)
                        | sigil_application::ApplicationCommandReceipt::Replayed(_)
                ) {
                    app.apply_saved_default_model(*root_config);
                }
                return Ok(());
            }
            #[cfg(test)]
            root_config.save_if_unchanged(&app.config_path, &expected_root_config)?;
            #[cfg(test)]
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
                Ok(None) => {
                    #[cfg(not(test))]
                    {
                        report_worker_unavailable(
                            app,
                            "application session creation requires the application port",
                        )?;
                        return Ok(());
                    }
                }
                Err(error) => {
                    report_worker_unavailable(
                        app,
                        &format!("application session creation was not admitted: {error}"),
                    )?;
                    return Ok(());
                }
            }
            #[cfg(test)]
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
            #[cfg(test)]
            let root_config = app
                .session_runtime_config_snapshot()
                .cloned()
                .context("new session requires the current runtime config")?;
            #[cfg(test)]
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
                let session = sigil_runtime::provider_connections::load_session_for_route(
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
            #[cfg(test)]
            let (attachment, session) = match preparation {
                Ok(prepared) => prepared,
                Err(error) => {
                    apply_worker_startup_recovery(app, &error, &session_log_path)?;
                    return Ok(());
                }
            };
            #[cfg(test)]
            let provider_name = session.provider_name().to_owned();
            #[cfg(test)]
            let model_name = session.model_name().to_owned();
            #[cfg(test)]
            let mut entries = session.entries().to_vec();
            #[cfg(test)]
            if let Err(error) = app.ensure_target_workspace_trust_decision_with_attachment(
                &session_log_path,
                &mut entries,
                "trusted workspace carried into new session",
                attachment.as_ref(),
            ) {
                apply_worker_startup_recovery(app, &error, &session_log_path)?;
                return Ok(());
            }
            #[cfg(test)]
            shutdown_and_join_worker(worker)?;
            #[cfg(test)]
            app.handle_worker_message(WorkerMessage::NewSessionStarted {
                session_id: session.session_scope_id().to_owned(),
                session_log_path: session_log_path.clone(),
                provider_name,
                model_name,
                entries,
            })?;
            #[cfg(test)]
            let _ = app.take_worker_rebind_required();
            #[cfg(test)]
            app.retain_worker_session_attachment(session_log_path, attachment);
            #[cfg(test)]
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
                Ok(None) => {
                    #[cfg(not(test))]
                    {
                        report_worker_unavailable(
                            app,
                            "application session switch requires the application port",
                        )?;
                        return Ok(());
                    }
                }
                Err(error) => {
                    report_worker_unavailable(
                        app,
                        &format!("application session switch was not admitted: {error}"),
                    )?;
                    return Ok(());
                }
            }
            #[cfg(test)]
            let attachment_recovery_binding = app
                .pending_session_attachment_recovery_binding_for(&session_log_path)
                .map(str::to_owned);
            #[cfg(test)]
            app.mark_pending_session_transition_target(session_log_path.clone());
            #[cfg(test)]
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
            #[cfg(test)]
            let root_config = app
                .session_runtime_config_snapshot()
                .cloned()
                .context("session switch requires the current runtime config")?;
            #[cfg(test)]
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
                let target = sigil_runtime::provider_connections::load_session_for_route(
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
            #[cfg(test)]
            let (target_attachment, target) = match preparation {
                Ok(prepared) => prepared,
                Err(error) => {
                    apply_worker_startup_recovery(app, &error, &session_log_path)?;
                    return Ok(());
                }
            };
            #[cfg(test)]
            let provider_name = target.provider_name().to_owned();
            #[cfg(test)]
            let model_name = target.model_name().to_owned();
            #[cfg(test)]
            let mut entries = target.entries().to_vec();
            #[cfg(test)]
            if let Err(error) = app.ensure_target_workspace_trust_decision_with_attachment(
                &session_log_path,
                &mut entries,
                "trusted workspace carried into restored session",
                target_attachment.as_ref(),
            ) {
                apply_worker_startup_recovery(app, &error, &session_log_path)?;
                return Ok(());
            }
            #[cfg(test)]
            shutdown_and_join_worker(worker)?;
            #[cfg(test)]
            app.handle_worker_message(WorkerMessage::SessionSwitched {
                session_id: target.session_scope_id().to_owned(),
                session_log_path: session_log_path.clone(),
                provider_name,
                model_name,
                entries,
            })?;
            #[cfg(test)]
            app.retain_worker_session_attachment(session_log_path, target_attachment);
            #[cfg(test)]
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
        action => {
            if queue_run_admission(app, worker, &action)? {
                return Ok(());
            }
            if queue_plan_revision(app, worker, &action)? {
                return Ok(());
            }
            if queue_application_interaction(app, worker, &action)? {
                return Ok(());
            } else {
                match try_execute_application_action(app, worker, &action) {
                    Ok(Some(receipt)) => {
                        report_application_action_receipt(app, &action, &receipt)?;
                    }
                    Ok(None) => {
                        let command = app.into_worker_command(action);
                        send_worker_command_with_restart(
                            app,
                            worker,
                            command,
                            &mut spawn_worker_fn,
                        )?;
                    }
                    Err(error) => {
                        report_application_admission_error(app, &action, &error)?;
                    }
                }
            }
        }
    }
    flush_pending_worker_commands(app, worker)?;
    Ok(())
}

fn apply_configuration_publication(
    app: &mut AppState,
    request: &Arc<crate::app::ConfigurationSaveRequest>,
) -> Result<Option<AppAction>> {
    let published_root_config = request
        .published_root_config
        .lock()
        .map_err(|_| anyhow::anyhow!("published config result lock poisoned"))?
        .take()
        .unwrap_or_else(|| request.next_base.clone());
    let owns_draft =
        app.accept_config_draft_publication(request.draft_binding, &published_root_config);
    app.apply_persisted_config_snapshot(&published_root_config);
    if !request.root_only
        && let Err(error) = app.apply_saved_provider_route_to_current_session(
            &request.expected,
            &published_root_config,
        )
    {
        report_worker_unavailable(
            app,
            &format!(
                "configuration was published but the selected session route could not be applied: {error:#}"
            ),
        )?;
        return Ok(None);
    }
    if request.close_after_save && owns_draft {
        app.close_config_panel_after_save();
    }
    Ok(match request.follow_up {
        crate::app::ConfigurationSaveFollowUp::RebootRuntime => Some(AppAction::ConfigSaved {
            root_config: Box::new(published_root_config),
        }),
        crate::app::ConfigurationSaveFollowUp::ApplyActiveRunPermissionMode(mode) => {
            Some(AppAction::UpdateActiveRunPermissionMode { mode })
        }
        crate::app::ConfigurationSaveFollowUp::ApplyPersistedDefaultModel => {
            app.apply_saved_default_model(published_root_config);
            None
        }
    })
}

pub(crate) struct PendingApplicationAdmission {
    application: Arc<application_bridge::TuiApplicationSession>,
    request: Arc<std::sync::Mutex<Option<sigil_application::ApplicationCommandRequest>>>,
    action: AppAction,
    run_admission: Option<application_bridge::TuiRunAdmission>,
    run_submission_intent: Option<Arc<()>>,
    queue_target: Option<sigil_kernel::ConversationInputTarget>,
    attachment_recovery_binding: Option<String>,
    retain_for_recovery: bool,
    settled: bool,
    refresh_before_prepare: bool,
    receiver: Option<
        std::sync::mpsc::Receiver<
            Result<
                sigil_application::ApplicationCommandReceipt,
                sigil_application::ApplicationError,
            >,
        >,
    >,
    handle: Option<std::thread::JoinHandle<()>>,
    retryable: bool,
    receipt_resolved: bool,
    reconcile_requested: bool,
}

impl std::fmt::Debug for PendingApplicationAdmission {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingApplicationAdmission")
            .field("receipt_resolved", &self.receipt_resolved)
            .field(
                "running",
                &self
                    .handle
                    .as_ref()
                    .is_some_and(|handle| !handle.is_finished()),
            )
            .finish_non_exhaustive()
    }
}

impl PendingApplicationAdmission {
    fn needs_polling(&self) -> bool {
        self.receiver.is_some() || self.reconcile_requested || self.handle.is_some()
    }
    fn run_observed(&self) -> bool {
        self.run_admission
            .as_ref()
            .is_some_and(|admission| admission.run_observed())
    }
    fn owns_current_run_submission(&self, app: &AppState) -> bool {
        self.run_submission_intent
            .as_ref()
            .is_none_or(|intent| Arc::ptr_eq(intent, &app.runtime.run_submission_intent))
    }
    fn receipt_resolved_and_finished(&self) -> bool {
        self.receipt_resolved
            && self
                .handle
                .as_ref()
                .is_none_or(|handle| handle.is_finished())
    }

    fn start(&mut self) -> Result<()> {
        if self.receiver.is_some() || !self.retryable {
            return Ok(());
        }
        if self
            .handle
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
        {
            return Ok(());
        }
        if let Some(handle) = self.handle.take() {
            wait_for_worker_thread(Some(handle), Instant::now())?;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let application = Arc::clone(&self.application);
        let request = Arc::clone(&self.request);
        let action = self.action.clone();
        let run_admission = self.run_admission.clone();
        let queue_target = self.queue_target.clone();
        let attachment_recovery_binding = self.attachment_recovery_binding.clone();
        let refresh_before_prepare = self.refresh_before_prepare;
        self.handle = Some(
            std::thread::Builder::new()
                .name("sigil-tui-admission".to_owned())
                .spawn(move || {
                    let result = (|| {
                        // Admission can outlive the UI event runtime. Own the executor and its
                        // blocking observations here, and drain them before publishing a result.
                        let runtime = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .map_err(|error| {
                                tracing::error!(%error, "failed to build TUI admission runtime");
                                sigil_application::ApplicationError::Unavailable
                            })?;
                        let _entered = runtime.enter();
                        let mut frozen = request
                            .lock()
                            .map_err(|_| sigil_application::ApplicationError::Unavailable)?;
                        if frozen.is_none() {
                            if refresh_before_prepare || application.current_projection()?.is_none()
                            {
                                runtime.block_on(application.refresh())?;
                            }
                            *frozen = Some(
                                application
                                    .prepare_action(
                                        &action,
                                        queue_target.as_ref(),
                                        attachment_recovery_binding.as_deref(),
                                    )?
                                    .ok_or(sigil_application::ApplicationError::Unavailable)?,
                            );
                        }
                        let request = frozen
                            .as_ref()
                            .ok_or(sigil_application::ApplicationError::Unavailable)?
                            .clone();
                        drop(frozen);
                        if let Some(admission) = run_admission {
                            runtime.block_on(application.execute_prepared_run(request, admission))
                        } else {
                            runtime.block_on(application.execute_prepared(request))
                        }
                    })();
                    let _ = sender.send(result);
                })
                .context("failed to start application admission")?,
        );
        self.receiver = Some(receiver);
        Ok(())
    }
}

fn is_run_admission_action(action: &AppAction) -> bool {
    matches!(
        action,
        AppAction::SubmitPrompt(_)
            | AppAction::SubmitPromptWithAttachments { .. }
            | AppAction::SubmitPlanPrompt(_)
            | AppAction::SubmitTask(_)
            | AppAction::ContinueTask { .. }
            | AppAction::InvokeInlineSkill { .. }
            | AppAction::InvokeChildSessionSkill { .. }
            | AppAction::InvokeAgentProfile { .. }
    )
}

fn has_owned_interaction_admission(action: &AppAction) -> bool {
    matches!(
        action,
        AppAction::QueueConversationInput { .. }
            | AppAction::CancelQueuedConversationInput { .. }
            | AppAction::EditQueuedConversationInput { .. }
            | AppAction::MoveQueuedConversationInput { .. }
            | AppAction::PromoteQueuedConversationInput { .. }
            | AppAction::SendQueuedConversationInputNow { .. }
            | AppAction::SetConversationQueuePaused { .. }
            | AppAction::SavePlan { .. }
            | AppAction::RejectPlan { .. }
            | AppAction::CreateTaskFromPlan { .. }
            | AppAction::SubmitUserInputDecision { .. }
            | AppAction::ResumeCommittedUserInput { .. }
            | AppAction::RevisePlan { .. }
    )
}

fn queue_background_application_action(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    action: &AppAction,
) -> Result<bool> {
    let mapped_action = application_bridge::TuiApplicationSession::supports_action(action)
        || matches!(action, AppAction::SetDefaultModel { .. });
    if mapped_action
        && !is_run_admission_action(action)
        && !has_owned_interaction_admission(action)
        && !matches!(
            action,
            AppAction::CancelRun | AppAction::SessionRuntimeRouteUpdated { .. }
        )
        && (!app.deferred_application_actions.is_empty()
            || background_action_must_wait(app, worker, action))
    {
        app.deferred_application_actions.push_back(action.clone());
        app.handle_worker_message(WorkerMessage::Notice(
            "operation queued; waiting for the preceding operation".to_owned(),
        ))?;
        return Ok(true);
    }
    let default_save;
    let action = if let AppAction::SetDefaultModel {
        root_config,
        expected_root_config,
    } = action
    {
        default_save = AppAction::PersistConfiguration {
            request: Arc::new(crate::app::ConfigurationSaveRequest {
                expected: *expected_root_config.clone(),
                next_base: *root_config.clone(),
                config_path: app.config_path.clone(),
                follow_up: crate::app::ConfigurationSaveFollowUp::ApplyPersistedDefaultModel,
                root_only: true,
                draft_binding: None,
                draft: std::sync::Mutex::new(None),
                published_root_config: std::sync::Mutex::new(None),
                close_after_save: false,
            }),
        };
        &default_save
    } else {
        action
    };
    if !application_bridge::TuiApplicationSession::supports_action(action)
        || is_run_admission_action(action)
        || has_owned_interaction_admission(action)
        || matches!(
            action,
            AppAction::CancelRun | AppAction::SessionRuntimeRouteUpdated { .. }
        )
    {
        return Ok(false);
    }
    let Some(runtime) = worker.as_mut() else {
        return Ok(false);
    };
    let Some(application) = runtime.application.as_ref() else {
        return Ok(false);
    };
    if runtime.pending_interactions.iter().any(|pending| {
        !pending.receipt_resolved && same_application_interaction(&pending.action, action)
    }) {
        return Ok(true);
    }
    if runtime.pending_interactions.len() >= MAX_PENDING_APPLICATION_INTERACTIONS {
        report_application_admission_error(
            app,
            action,
            &anyhow::anyhow!("too many application operations are awaiting a result"),
        )?;
        return Ok(true);
    }
    let attachment_recovery_binding = if let AppAction::SwitchSession { session_log_path } = action
    {
        app.pending_session_attachment_recovery_binding_for(session_log_path)
            .map(str::to_owned)
    } else {
        None
    };
    let mut pending = PendingApplicationAdmission {
        application: Arc::clone(application),
        request: Arc::new(std::sync::Mutex::new(None)),
        action: action.clone(),
        run_admission: None,
        run_submission_intent: None,
        queue_target: app.active_conversation_queue_target(),
        attachment_recovery_binding,
        retain_for_recovery: false,
        settled: false,
        refresh_before_prepare: false,
        receiver: None,
        handle: None,
        retryable: true,
        receipt_resolved: false,
        reconcile_requested: false,
    };
    if let Err(error) = pending.start() {
        report_application_admission_error(app, action, &error)?;
        return Ok(true);
    }
    runtime.pending_interactions.push(pending);
    app.handle_worker_message(WorkerMessage::Notice("submitting operation".to_owned()))?;
    Ok(true)
}

fn is_application_lifecycle_action(action: &AppAction) -> bool {
    matches!(
        action,
        AppAction::StartNewSession { .. }
            | AppAction::SwitchSession { .. }
            | AppAction::PersistConfiguration { .. }
            | AppAction::SetDefaultModel { .. }
    )
}

fn background_action_must_wait(
    app: &AppState,
    worker: &Option<WorkerRuntime>,
    action: &AppAction,
) -> bool {
    app.runtime.worker_rebind_required
        || app
            .runtime_transition
            .as_ref()
            .is_some_and(|owner| owner.is_running())
        || app
            .runtime_maintenance
            .as_ref()
            .is_some_and(|owner| owner.is_running())
        || worker.as_ref().is_some_and(|runtime| {
            !runtime.ready
                || runtime.pending_interactions.iter().any(|pending| {
                    is_application_lifecycle_action(&pending.action)
                        || (is_application_lifecycle_action(action)
                            && (pending.receiver.is_some() || pending.handle.is_some()))
                })
                || (is_application_lifecycle_action(action)
                    && runtime.pending_admission.as_ref().is_some_and(|pending| {
                        pending.receiver.is_some() || pending.handle.is_some()
                    }))
        })
}

fn flush_deferred_application_action(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
) -> Result<bool> {
    let Some(action) = app.deferred_application_actions.front() else {
        return Ok(false);
    };
    // These are UI intents, not frozen application requests. Prepare them only against the
    // replacement worker's application identity after its Ready handoff.
    if worker
        .as_ref()
        .is_none_or(|runtime| !runtime.ready || runtime.application.is_none())
        || background_action_must_wait(app, worker, action)
    {
        return Ok(false);
    }
    let action = app
        .deferred_application_actions
        .pop_front()
        .expect("front checked");
    let remaining = std::mem::take(&mut app.deferred_application_actions);
    let result = queue_background_application_action(app, worker, &action);
    app.deferred_application_actions.extend(remaining);
    anyhow::ensure!(result?, "deferred application action lost its dispatcher");
    Ok(true)
}

fn queue_run_admission(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    action: &AppAction,
) -> Result<bool> {
    if !is_run_admission_action(action) {
        return Ok(false);
    }
    let Some(runtime) = worker.as_mut() else {
        return Ok(false);
    };
    let Some(application) = runtime.application.as_ref() else {
        return Ok(false);
    };
    if let Some(pending) = runtime.pending_interactions.iter_mut().find(|pending| {
        !pending.receipt_resolved
            && !pending.run_observed()
            && pending.owns_current_run_submission(app)
            && same_application_interaction(&pending.action, action)
    }) {
        if let Err(error) = pending.start() {
            report_application_admission_error(app, action, &error)?;
        }
        return Ok(true);
    }
    if runtime.pending_interactions.len() >= MAX_PENDING_APPLICATION_INTERACTIONS {
        report_application_admission_error(
            app,
            action,
            &anyhow::anyhow!("too many application operations are awaiting a result"),
        )?;
        return Ok(true);
    }
    // Freeze cancellation and attachment identity before yielding to admission. Projection
    // refresh, validation and durable reservation belong to the owned thread, not the UI frame.
    let admission = application.reserve_run_admission(&runtime.worker_tx)?;
    let mut pending = PendingApplicationAdmission {
        application: Arc::clone(application),
        request: Arc::new(std::sync::Mutex::new(None)),
        action: action.clone(),
        run_admission: Some(admission),
        run_submission_intent: Some(Arc::clone(&app.runtime.run_submission_intent)),
        queue_target: None,
        attachment_recovery_binding: None,
        retain_for_recovery: true,
        settled: false,
        refresh_before_prepare: false,
        receiver: None,
        handle: None,
        retryable: true,
        receipt_resolved: false,
        reconcile_requested: false,
    };
    if let Err(error) = pending.start() {
        report_application_admission_error(app, action, &error)?;
        return Ok(true);
    }
    runtime.pending_interactions.push(pending);
    Ok(true)
}

fn queue_plan_revision(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    action: &AppAction,
) -> Result<bool> {
    let AppAction::RevisePlan {
        plan_id,
        expected_plan_hash,
    } = action
    else {
        return Ok(false);
    };
    let Some(runtime) = worker.as_mut() else {
        report_application_admission_error(
            app,
            action,
            &anyhow::anyhow!("plan revision requires an attached worker"),
        )?;
        return Ok(true);
    };
    let Some(application) = runtime.application.as_ref() else {
        report_application_admission_error(
            app,
            action,
            &anyhow::anyhow!("plan revision requires an attached application projection"),
        )?;
        return Ok(true);
    };
    if runtime
        .pending_admission
        .as_ref()
        .is_some_and(|pending| pending.receipt_resolved)
    {
        if runtime.pending_interactions.len() >= MAX_PENDING_APPLICATION_INTERACTIONS {
            report_application_admission_error(
                app,
                action,
                &anyhow::anyhow!("previous plan operation is still finishing"),
            )?;
            return Ok(true);
        }
        if let Some(pending) = runtime.pending_admission.take() {
            runtime.pending_interactions.push(pending);
        }
    }
    if let Some(pending) = runtime.pending_admission.as_mut() {
        if !same_application_interaction(&pending.action, action) {
            report_application_admission_error(
                app,
                action,
                &anyhow::anyhow!("another plan revision is awaiting its result"),
            )?;
            return Ok(true);
        }
        if let Err(error) = pending.start() {
            report_application_admission_error(app, action, &error)?;
            return Ok(true);
        }
        app.handle_worker_message(WorkerMessage::Notice(
            "plan revision is pending; waiting for its durable outcome".to_owned(),
        ))?;
        return Ok(true);
    }
    let request =
        match application.prepare_plan_revision(plan_id.clone(), expected_plan_hash.clone()) {
            Ok(request) => request,
            Err(error) => {
                report_application_admission_error(app, action, &anyhow::Error::new(error))?;
                return Ok(true);
            }
        };
    let mut pending = PendingApplicationAdmission {
        application: Arc::clone(application),
        request: Arc::new(std::sync::Mutex::new(Some(request))),
        action: action.clone(),
        run_admission: None,
        run_submission_intent: None,
        queue_target: None,
        attachment_recovery_binding: None,
        retain_for_recovery: true,
        settled: false,
        refresh_before_prepare: false,
        receiver: None,
        handle: None,
        retryable: true,
        receipt_resolved: false,
        reconcile_requested: false,
    };
    if let Err(error) = pending.start() {
        report_application_admission_error(app, action, &error)?;
        return Ok(true);
    }
    runtime.pending_admission = Some(pending);
    app.handle_worker_message(WorkerMessage::Notice("opening plan revision".to_owned()))?;
    Ok(true)
}

const MAX_PENDING_APPLICATION_INTERACTIONS: usize = 32;

fn queue_application_interaction(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    action: &AppAction,
) -> Result<bool> {
    if !matches!(
        action,
        AppAction::QueueConversationInput { .. }
            | AppAction::CancelQueuedConversationInput { .. }
            | AppAction::EditQueuedConversationInput { .. }
            | AppAction::MoveQueuedConversationInput { .. }
            | AppAction::PromoteQueuedConversationInput { .. }
            | AppAction::SendQueuedConversationInputNow { .. }
            | AppAction::SetConversationQueuePaused { .. }
            | AppAction::SavePlan { .. }
            | AppAction::RejectPlan { .. }
            | AppAction::CreateTaskFromPlan { .. }
            | AppAction::SubmitUserInputDecision { .. }
            | AppAction::ResumeCommittedUserInput { .. }
    ) {
        return Ok(false);
    }
    let resume = matches!(action, AppAction::ResumeCommittedUserInput { .. });
    let Some(runtime) = worker.as_mut() else {
        if resume {
            report_application_admission_error(
                app,
                action,
                &anyhow::anyhow!("user input continuation requires an attached application owner"),
            )?;
        }
        return Ok(resume);
    };
    let Some(application) = runtime.application.as_ref() else {
        if resume {
            report_application_admission_error(
                app,
                action,
                &anyhow::anyhow!("user input continuation requires an attached application owner"),
            )?;
        }
        return Ok(resume);
    };
    let waiting_for_run = matches!(action, AppAction::QueueConversationInput { .. })
        && runtime.pending_interactions.iter().any(|pending| {
            pending.run_admission.is_some() && !pending.run_observed() && pending.receiver.is_some()
        });
    if let Some(pending) = runtime.pending_interactions.iter_mut().find(|pending| {
        !pending.receipt_resolved && same_application_interaction(&pending.action, action)
    }) {
        if !waiting_for_run && let Err(error) = pending.start() {
            report_application_admission_error(app, action, &error)?;
        }
        return Ok(true);
    }
    if runtime.pending_interactions.len() >= MAX_PENDING_APPLICATION_INTERACTIONS {
        report_application_admission_error(
            app,
            action,
            &anyhow::anyhow!("too many application operations are awaiting a result"),
        )?;
        return Ok(true);
    }
    // These actions prepare against the cached projection. Durable admission and dispatch run
    // on the owned thread so pending feedback and urgent cancellation stay responsive.
    let request = if resume || waiting_for_run || application.current_projection()?.is_none() {
        None
    } else {
        match application.prepare_action(
            action,
            app.active_conversation_queue_target().as_ref(),
            None,
        ) {
            Ok(Some(request)) => Some(request),
            Ok(None) => return Ok(false),
            Err(error) => {
                report_application_admission_error(app, action, &anyhow::Error::new(error))?;
                return Ok(true);
            }
        }
    };
    let mut pending = PendingApplicationAdmission {
        application: Arc::clone(application),
        request: Arc::new(std::sync::Mutex::new(request)),
        action: action.clone(),
        run_admission: None,
        run_submission_intent: None,
        queue_target: app.active_conversation_queue_target(),
        attachment_recovery_binding: None,
        retain_for_recovery: true,
        settled: false,
        refresh_before_prepare: waiting_for_run,
        receiver: None,
        handle: None,
        retryable: true,
        receipt_resolved: false,
        reconcile_requested: false,
    };
    if waiting_for_run {
        pending.reconcile_requested = true;
    } else if let Err(error) = pending.start() {
        report_application_admission_error(app, action, &error)?;
        return Ok(true);
    }
    runtime.pending_interactions.push(pending);
    Ok(true)
}

fn same_application_interaction(left: &AppAction, right: &AppAction) -> bool {
    if let Some(operation) = AppState::queue_operation_for_action(left) {
        return AppState::queue_operation_for_action(right).as_ref() == Some(&operation);
    }
    match (left, right) {
        (
            AppAction::StartNewSession {
                session_log_path: a,
            },
            AppAction::StartNewSession {
                session_log_path: b,
            },
        )
        | (
            AppAction::SwitchSession {
                session_log_path: a,
            },
            AppAction::SwitchSession {
                session_log_path: b,
            },
        ) => a == b,
        (
            AppAction::PersistConfiguration { request: a },
            AppAction::PersistConfiguration { request: b },
        ) => Arc::ptr_eq(a, b),
        (AppAction::SubmitPrompt(a), AppAction::SubmitPrompt(b))
        | (AppAction::SubmitPlanPrompt(a), AppAction::SubmitPlanPrompt(b))
        | (AppAction::SubmitTask(a), AppAction::SubmitTask(b)) => a == b,
        (
            AppAction::SubmitPromptWithAttachments {
                prompt: a,
                attachments: b,
            },
            AppAction::SubmitPromptWithAttachments {
                prompt: c,
                attachments: d,
            },
        ) => a == c && b == d,
        (
            AppAction::ContinueTask {
                task_id: a,
                guidance: b,
            },
            AppAction::ContinueTask {
                task_id: c,
                guidance: d,
            },
        ) => a == c && b == d,
        (
            AppAction::InvokeInlineSkill {
                skill_id: a,
                arguments: b,
                attachments: e,
            },
            AppAction::InvokeInlineSkill {
                skill_id: c,
                arguments: d,
                attachments: f,
            },
        ) => a == c && b == d && e == f,
        (
            AppAction::InvokeChildSessionSkill {
                skill_id: a,
                arguments: b,
            },
            AppAction::InvokeChildSessionSkill {
                skill_id: c,
                arguments: d,
            },
        ) => a == c && b == d,
        (
            AppAction::InvokeAgentProfile {
                profile_id: a,
                prompt: b,
                parent_prompt: e,
            },
            AppAction::InvokeAgentProfile {
                profile_id: c,
                prompt: d,
                parent_prompt: f,
            },
        ) => a == c && b == d && e == f,
        (
            AppAction::SavePlan {
                plan_id: left_id,
                expected_plan_hash: left_hash,
            },
            AppAction::SavePlan {
                plan_id: right_id,
                expected_plan_hash: right_hash,
            },
        )
        | (
            AppAction::RevisePlan {
                plan_id: left_id,
                expected_plan_hash: left_hash,
            },
            AppAction::RevisePlan {
                plan_id: right_id,
                expected_plan_hash: right_hash,
            },
        )
        | (
            AppAction::RejectPlan {
                plan_id: left_id,
                expected_plan_hash: left_hash,
            },
            AppAction::RejectPlan {
                plan_id: right_id,
                expected_plan_hash: right_hash,
            },
        ) => left_id == right_id && left_hash == right_hash,
        (
            AppAction::CreateTaskFromPlan {
                plan_id: left_id,
                expected_plan_hash: left_hash,
                start_mode: left_mode,
                permission_grant: left_grant,
            },
            AppAction::CreateTaskFromPlan {
                plan_id: right_id,
                expected_plan_hash: right_hash,
                start_mode: right_mode,
                permission_grant: right_grant,
            },
        ) => {
            left_id == right_id
                && left_hash == right_hash
                && left_mode == right_mode
                && left_grant == right_grant
        }
        (
            AppAction::ResumeCommittedUserInput {
                original_command_id: a,
                request_id: b,
                generation: c,
                expected_request_hash: d,
            },
            AppAction::ResumeCommittedUserInput {
                original_command_id: w,
                request_id: x,
                generation: y,
                expected_request_hash: z,
            },
        ) => a == w && b == x && c == y && d == z,
        (
            AppAction::SubmitUserInputDecision {
                command_id: left_command,
                request_id: left_id,
                generation: left_generation,
                expected_request_hash: left_hash,
                decision: left_decision,
            },
            AppAction::SubmitUserInputDecision {
                command_id: right_command,
                request_id: right_id,
                generation: right_generation,
                expected_request_hash: right_hash,
                decision: right_decision,
            },
        ) => {
            left_command == right_command
                && left_id == right_id
                && left_generation == right_generation
                && left_hash == right_hash
                && left_decision == right_decision
        }
        _ => false,
    }
}

fn application_interaction_outcome_matches(action: &AppAction, message: &WorkerMessage) -> bool {
    if let WorkerMessage::ConversationQueueOperationCompleted { operation, .. } = message {
        return AppState::queue_operation_for_action(action).as_ref() == Some(operation);
    }
    match (action, message) {
        (AppAction::SubmitPrompt(a), WorkerMessage::RunStarted { prompt: b })
        | (
            AppAction::SubmitPromptWithAttachments { prompt: a, .. },
            WorkerMessage::RunStarted { prompt: b },
        )
        | (AppAction::SubmitPlanPrompt(a), WorkerMessage::PlanRunStarted { prompt: b })
        | (AppAction::SubmitTask(a), WorkerMessage::TaskRunStarted { objective: b, .. }) => a == b,
        (
            AppAction::InvokeInlineSkill { skill_id: a, .. }
            | AppAction::InvokeChildSessionSkill { skill_id: a, .. },
            WorkerMessage::SkillRunStarted { skill_id: b, .. },
        ) => a == b,
        (
            AppAction::InvokeAgentProfile {
                profile_id: a,
                prompt: b,
                ..
            },
            WorkerMessage::AgentRunStarted {
                profile_id: c,
                prompt: d,
            },
        ) => a == c && b == d,
        (
            AppAction::ContinueTask { task_id, .. },
            WorkerMessage::TaskRunStarted {
                task_id: started, ..
            },
        ) => task_id.as_ref().is_none_or(|expected| expected == started),
        (
            AppAction::RevisePlan {
                plan_id,
                expected_plan_hash,
            },
            WorkerMessage::UserInputRequested { request, .. },
        ) => matches!(
            &request.source,
            sigil_kernel::UserInputSourceV1::PlanRevision { base_plan_id, base_plan_hash }
                if plan_id == base_plan_id.as_str() && expected_plan_hash == base_plan_hash
        ),
        (
            AppAction::RevisePlan {
                plan_id,
                expected_plan_hash,
            },
            WorkerMessage::PlanActionFailed {
                action: sigil_kernel::PublicPlanAction::Revise,
                plan_id: failed_id,
                expected_plan_hash: failed_hash,
                ..
            },
        ) => plan_id == failed_id && expected_plan_hash == failed_hash,
        (
            AppAction::SavePlan {
                plan_id,
                expected_plan_hash,
            },
            WorkerMessage::PlanSaved { entry, .. },
        )
        | (
            AppAction::RejectPlan {
                plan_id,
                expected_plan_hash,
            },
            WorkerMessage::PlanRejected { entry, .. },
        ) => plan_id == entry.plan_id.as_str() && expected_plan_hash == &entry.plan_hash,
        (
            AppAction::CreateTaskFromPlan {
                plan_id,
                expected_plan_hash,
                ..
            },
            WorkerMessage::TaskCreatedFromPlan { entry, .. },
        ) => plan_id == entry.plan_id.as_str() && expected_plan_hash == &entry.plan_hash,
        (
            AppAction::SavePlan {
                plan_id,
                expected_plan_hash,
            },
            WorkerMessage::PlanActionFailed {
                action: sigil_kernel::PublicPlanAction::Save,
                plan_id: failed_id,
                expected_plan_hash: failed_hash,
                ..
            },
        )
        | (
            AppAction::RejectPlan {
                plan_id,
                expected_plan_hash,
            },
            WorkerMessage::PlanActionFailed {
                action: sigil_kernel::PublicPlanAction::Reject,
                plan_id: failed_id,
                expected_plan_hash: failed_hash,
                ..
            },
        )
        | (
            AppAction::CreateTaskFromPlan {
                plan_id,
                expected_plan_hash,
                ..
            },
            WorkerMessage::PlanActionFailed {
                action: sigil_kernel::PublicPlanAction::Run,
                plan_id: failed_id,
                expected_plan_hash: failed_hash,
                ..
            },
        ) => plan_id == failed_id && expected_plan_hash == failed_hash,
        (
            AppAction::SubmitUserInputDecision {
                request_id,
                generation,
                expected_request_hash,
                ..
            }
            | AppAction::ResumeCommittedUserInput {
                request_id,
                generation,
                expected_request_hash,
                ..
            },
            WorkerMessage::UserInputDecisionApplied { request, .. },
        ) => {
            request_id == request.identity.request_id.as_str()
                && generation == &request.identity.generation
                && expected_request_hash == &request.request_hash
        }
        (
            AppAction::SubmitUserInputDecision {
                request_id,
                generation,
                expected_request_hash,
                ..
            }
            | AppAction::ResumeCommittedUserInput {
                request_id,
                generation,
                expected_request_hash,
                ..
            },
            WorkerMessage::UserInputDecisionFailed {
                request_id: failed_id,
                generation: failed_generation,
                expected_request_hash: failed_hash,
                ..
            },
        ) => {
            request_id == failed_id
                && generation == failed_generation
                && expected_request_hash == failed_hash
        }
        _ => false,
    }
}

fn poll_application_admission(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
) -> Result<bool> {
    let mut changed = false;
    let Some(runtime) = worker.as_mut() else {
        return Ok(changed);
    };
    let mut configuration_publications = Vec::new();
    if let Some(pending) = runtime.pending_admission.as_mut() {
        changed |= poll_pending_application_admission(app, pending)?;
        if pending.receipt_resolved_and_finished() {
            wait_for_worker_thread(pending.handle.take(), Instant::now())?;
            runtime.pending_admission = None;
            changed = true;
        }
    }
    let mut index = 0;
    while index < runtime.pending_interactions.len() {
        let waiting_for_run = runtime.pending_interactions.iter().any(|pending| {
            pending.run_admission.is_some() && !pending.run_observed() && pending.receiver.is_some()
        });
        let pending = &mut runtime.pending_interactions[index];
        if waiting_for_run
            && matches!(pending.action, AppAction::QueueConversationInput { .. })
            && pending.receiver.is_none()
        {
            index += 1;
            continue;
        }
        changed |= poll_pending_application_admission(app, pending)?;
        if pending.receipt_resolved_and_finished() {
            wait_for_worker_thread(pending.handle.take(), Instant::now())?;
            if pending.settled
                && let AppAction::PersistConfiguration { request } = &pending.action
            {
                configuration_publications.push(Arc::clone(request));
            }
            runtime.pending_interactions.remove(index);
            changed = true;
        } else {
            index += 1;
        }
    }
    for request in configuration_publications {
        if let Some(action) = apply_configuration_publication(app, &request)? {
            #[cfg(not(test))]
            process_app_action_with_spawner_and_host(
                app,
                worker,
                action,
                spawn_worker,
                &mut SystemHostEffects,
            )?;
            #[cfg(test)]
            process_app_action(app, worker, action)?;
        }
    }
    Ok(changed)
}

fn poll_pending_application_admission(
    app: &mut AppState,
    pending: &mut PendingApplicationAdmission,
) -> Result<bool> {
    let joined = pending
        .handle
        .as_ref()
        .is_some_and(|handle| handle.is_finished());
    if joined {
        wait_for_worker_thread(pending.handle.take(), Instant::now())?;
    }
    if pending.receipt_resolved {
        return Ok(joined);
    }
    if pending.receiver.is_none() && pending.reconcile_requested {
        if matches!(pending.action, AppAction::QueueConversationInput { .. }) {
            // A deferred follow-up keeps its explicit target in the action. Revalidate against
            // the now-active target; never silently retarget it after the preceding admission.
            pending.queue_target = app.active_conversation_queue_target();
        }
        pending.retryable = true;
        pending.start()?;
        if pending.receiver.is_some() {
            pending.reconcile_requested = false;
        }
    }
    let Some(receiver) = pending.receiver.as_ref() else {
        return Ok(joined);
    };
    let result = match receiver.try_recv() {
        Ok(result) => result,
        Err(std::sync::mpsc::TryRecvError::Empty) => return Ok(joined),
        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
            Err(sigil_application::ApplicationError::Unavailable)
        }
    };
    pending.receiver = None;
    match result {
        Ok(receipt) => {
            pending.retryable = false;
            pending.settled = matches!(
                receipt,
                sigil_application::ApplicationCommandReceipt::Settled(_)
                    | sigil_application::ApplicationCommandReceipt::Replayed(_)
            );
            let resolved = matches!(
                receipt,
                sigil_application::ApplicationCommandReceipt::Settled(_)
                    | sigil_application::ApplicationCommandReceipt::Replayed(_)
                    | sigil_application::ApplicationCommandReceipt::Rejected(_)
                    | sigil_application::ApplicationCommandReceipt::ConfirmedNoEffect(_)
                    | sigil_application::ApplicationCommandReceipt::PayloadConflict(_)
            );
            if pending.run_observed() || !pending.owns_current_run_submission(app) {
                report_application_receipt(app, &receipt)?;
            } else {
                report_application_action_receipt(app, &pending.action, &receipt)?;
            }
            if resolved || !pending.retain_for_recovery {
                pending.receipt_resolved = true;
            }
        }
        Err(error) => {
            pending.retryable = true;
            pending.receipt_resolved = !pending.retain_for_recovery;
            if pending.run_observed() || !pending.owns_current_run_submission(app) {
                app.handle_worker_message(WorkerMessage::Notice(format!(
                    "run admission receipt is delayed: {error}"
                )))?;
            } else {
                report_application_admission_error(
                    app,
                    &pending.action,
                    &anyhow::Error::new(error),
                )?;
            }
        }
    }
    Ok(true)
}

fn try_execute_application_action(
    app: &AppState,
    worker: &Option<WorkerRuntime>,
    action: &AppAction,
) -> Result<Option<sigil_application::ApplicationCommandReceipt>> {
    #[cfg(not(test))]
    {
        let application = worker
            .as_ref()
            .and_then(|runtime| runtime.application.clone())
            .or_else(|| {
                app.runtime_transition
                    .as_ref()
                    .and_then(|owner| owner.application())
            });
        let Some(application) = application else {
            return Ok(None);
        };
        let attachment_recovery_binding = match action {
            AppAction::SwitchSession { session_log_path } => {
                app.pending_session_attachment_recovery_binding_for(session_log_path)
            }
            _ => None,
        };
        Ok(application.try_execute_action(
            action,
            app.active_conversation_queue_target().as_ref(),
            attachment_recovery_binding,
        )?)
    }
    #[cfg(test)]
    {
        let _ = (app, worker, action);
        Ok(None)
    }
}

#[cfg(not(test))]
async fn refresh_application_projection_task(
    application: Arc<application_bridge::TuiApplicationSession>,
) -> std::result::Result<ProjectionRefreshOutcome, sigil_application::ApplicationError> {
    let projection = application.refresh().await?;
    Ok(ProjectionRefreshOutcome { projection })
}

#[cfg(not(test))]
fn report_projection_failure(
    app: &mut AppState,
    operation: &str,
    error: &ProjectionFailure,
    retrying: bool,
) -> Result<()> {
    let message = if retrying {
        format!("{operation} is delayed; retrying safely. {error}")
    } else {
        format!("{operation} failed: {error}")
    };
    app.handle_worker_message(WorkerMessage::Notice(sigil_kernel::safe_persistence_text(
        &message,
    )))
}

#[cfg(not(test))]
fn report_projection_recovered(app: &mut AppState, operation: &str) -> Result<()> {
    app.handle_worker_message(WorkerMessage::Notice(format!("{operation} recovered")))
}

#[cfg(not(test))]
fn retain_pending_projection_ack(
    pending: &mut Option<PendingProjectionAck>,
    candidate: PendingProjectionAck,
    epoch: u64,
) {
    if should_replace_pending_ack(
        epoch,
        pending
            .as_ref()
            .map(|pending| (pending.epoch, &pending.frontier)),
        candidate.epoch,
        &candidate.frontier,
    ) {
        *pending = Some(candidate);
    } else if let Some(existing) = pending.as_mut()
        && existing.epoch == candidate.epoch
        && existing.frontier == candidate.frontier
    {
        for event_id in candidate.event_ids {
            if !existing.event_ids.contains(&event_id) {
                existing.event_ids.push(event_id);
            }
        }
    }
}

fn worker_message_requires_projection_refresh(message: &WorkerMessage) -> bool {
    match message {
        WorkerMessage::WorkerReady
        | WorkerMessage::SessionRouteRecoveryRequired { .. }
        | WorkerMessage::ApprovalCommandReceipt(_)
        | WorkerMessage::RunStarted { .. }
        | WorkerMessage::SkillRunStarted { .. }
        | WorkerMessage::PlanRunStarted { .. }
        | WorkerMessage::AgentRunStarted { .. }
        | WorkerMessage::AgentResultContinuationStarted { .. }
        | WorkerMessage::ConversationQueueUpdated { .. }
        | WorkerMessage::ConversationQueueDispatchStarted { .. }
        | WorkerMessage::AgentRunFinished { .. }
        | WorkerMessage::RunFinished { .. }
        | WorkerMessage::PlanRunFinished { .. }
        | WorkerMessage::PlanReviewBlocked { .. }
        | WorkerMessage::UserInputRequested { .. }
        | WorkerMessage::RecoveredUserInputAttention { .. }
        | WorkerMessage::UserInputDecisionApplied { .. }
        | WorkerMessage::PlanRejected { .. }
        | WorkerMessage::PlanSaved { .. }
        | WorkerMessage::TaskCreatedFromPlan { .. }
        | WorkerMessage::PlanActionFailed { .. }
        | WorkerMessage::TaskRunFinished { .. }
        | WorkerMessage::TaskRunPaused { .. }
        | WorkerMessage::TaskRunStarted { .. }
        | WorkerMessage::RunCancelled { .. }
        | WorkerMessage::RunInterrupted { .. }
        | WorkerMessage::TerminalTaskUpdated { .. }
        | WorkerMessage::AgentThreadClosed { .. }
        | WorkerMessage::AgentThreadCancelled { .. }
        | WorkerMessage::SessionSwitched { .. }
        | WorkerMessage::NewSessionStarted { .. }
        | WorkerMessage::V2CompactionApplied { .. }
        | WorkerMessage::StandaloneToolOutputShrinkApplied { .. }
        | WorkerMessage::IntentDropCompleted { .. }
        | WorkerMessage::TaskIntegrationAccepted { .. }
        | WorkerMessage::TaskIntegrationAcceptanceFailed { .. }
        | WorkerMessage::CheckpointRestoreCompleted { .. }
        | WorkerMessage::ConversationForked { .. }
        | WorkerMessage::LocalSessionForked { .. }
        | WorkerMessage::ToolArtifactPageRead { .. }
        | WorkerMessage::ToolArtifactPageReadFailed { .. }
        | WorkerMessage::LocalSessionDeleted { .. }
        | WorkerMessage::SessionRetentionApplied { .. }
        | WorkerMessage::RunFailed(_)
        | WorkerMessage::SessionAttachmentTransferred { .. } => true,
        WorkerMessage::Event(event) | WorkerMessage::AgentThreadEvent { event, .. } => {
            run_event_requires_projection_refresh(event)
        }
        _ => false,
    }
}

fn run_event_requires_projection_refresh(event: &sigil_kernel::RunEvent) -> bool {
    matches!(
        event,
        sigil_kernel::RunEvent::ToolApprovalRequested { .. }
            | sigil_kernel::RunEvent::ToolApprovalResolved { .. }
            | sigil_kernel::RunEvent::ToolResult(_)
            | sigil_kernel::RunEvent::ContinuationState(_)
            | sigil_kernel::RunEvent::ProviderTurnRecovery(_)
            | sigil_kernel::RunEvent::ProviderTurnPartialOutputDiscarded(_)
            | sigil_kernel::RunEvent::Control(_)
            | sigil_kernel::RunEvent::AssistantMessage(_)
    )
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
        sigil_application::ApplicationCommandReceipt::ConfirmedNoEffect(_) => {
            "application command was confirmed to have no effect"
        }
        sigil_application::ApplicationCommandReceipt::SafetyStopRequestedButUnrecorded(_) => {
            "safety stop requested; durable cancellation is not yet recorded"
        }
    };
    app.handle_worker_message(WorkerMessage::Notice(notice.to_owned()))
}

fn report_application_action_receipt(
    app: &mut AppState,
    action: &AppAction,
    receipt: &sigil_application::ApplicationCommandReceipt,
) -> Result<()> {
    use sigil_application::ApplicationCommandReceipt;

    let failure = match receipt {
        ApplicationCommandReceipt::Rejected(rejection) => Some(format!(
            "application command rejected: {}",
            rejection.reason
        )),
        ApplicationCommandReceipt::PayloadConflict(_) => {
            Some("application command payload conflicts with its durable reservation".to_owned())
        }
        ApplicationCommandReceipt::ConfirmedNoEffect(_) => {
            Some("application command was confirmed to have no effect".to_owned())
        }
        _ => None,
    };
    if let Some(failure) = failure {
        fail_pending_application_action(app, action, &failure);
    }
    report_application_receipt(app, receipt)
}

fn fail_pending_application_action(app: &mut AppState, action: &AppAction, message: &str) {
    if is_run_admission_action(action) && app.approval.pending.is_none() {
        app.restore_unadmitted_run_input(action);
    }
    let message = sigil_kernel::safe_persistence_text(message);
    if app.fail_queue_action(action, message.clone())
        || app.fail_plan_action(action, message.clone())
    {
        return;
    }
    if let AppAction::SubmitUserInputDecision {
        request_id,
        generation,
        expected_request_hash,
        ..
    }
    | AppAction::ResumeCommittedUserInput {
        request_id,
        generation,
        expected_request_hash,
        ..
    } = action
    {
        app.fail_pending_user_input_submission(
            request_id,
            *generation,
            expected_request_hash,
            message,
        );
    }
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

/// Returns the product to the only state that can repair a failed authority boot. A failed
/// composition is not a provider outage: there is no worker or authority-backed session to
/// recover, so keeping the normal composer mounted would create a false-ready UI and queue
/// commands that cannot ever be delivered.
fn return_to_setup_after_boot_failure(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    _config_path: PathBuf,
    startup_error: String,
    startup_recovery_code: Option<sigil_kernel::PublicRouteRecoveryCode>,
) -> Result<()> {
    return_to_setup_after_boot_failure_with_draft(
        app,
        worker,
        None,
        startup_error,
        startup_recovery_code,
    )
}

fn return_to_setup_after_boot_failure_with_draft(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    mut setup_draft: Option<crate::setup::SetupState>,
    startup_error: String,
    startup_recovery_code: Option<sigil_kernel::PublicRouteRecoveryCode>,
) -> Result<()> {
    let support_build_info = app.support_build_info().clone();
    let update_build_info = app.update_build_info().clone();
    let workspace_root = app.workspace_root.clone();
    shutdown_and_join_worker(worker)?;
    let config_path = setup_draft
        .as_ref()
        .map(|draft| draft.config_path.clone())
        .unwrap_or_else(|| app.config_path.clone());
    let replacement = AppState::from_setup_with_recovery(
        config_path,
        workspace_root,
        Some(startup_error.clone()),
        startup_recovery_code,
    );
    control_log_recovery::replace_app_state(app, replacement);
    if let Some(draft) = setup_draft.as_mut() {
        draft.startup_error = Some(startup_error);
        draft.startup_recovery_code = startup_recovery_code;
        draft.save_error = None;
    }
    if let Some(draft) = setup_draft {
        // Keep the values and selected review row that the user just submitted. A failed
        // authority retry must be recoverable in place, not silently reset to the Provider row.
        app.restore_setup_state(draft);
    }
    app.set_support_build_info(support_build_info);
    app.set_update_build_info(update_build_info);
    Ok(())
}

fn startup_recovery_code_from_error(
    error: &anyhow::Error,
) -> Option<sigil_kernel::PublicRouteRecoveryCode> {
    let boot_error = error.chain().find_map(|cause| {
        cause.downcast_ref::<sigil_runtime::application_host::BootAuthorityErrorV1>()
    })?;
    match boot_error {
        sigil_runtime::application_host::BootAuthorityErrorV1::Composition(_)
        | sigil_runtime::application_host::BootAuthorityErrorV1::Cutover(_)
        | sigil_runtime::application_host::BootAuthorityErrorV1::Bootstrap(_) => {
            Some(sigil_kernel::PublicRouteRecoveryCode::AuthorityUnavailable)
        }
        sigil_runtime::application_host::BootAuthorityErrorV1::Config(_) => None,
    }
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
    drain_worker_messages_inner(app, worker, None).map(|(dirty, _)| dirty)
}

#[cfg(not(test))]
fn drain_worker_messages_with_attention(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    attention: &mut AttentionController,
) -> Result<(bool, bool)> {
    drain_worker_messages_inner(app, worker, Some(attention))
}

fn drain_worker_messages_inner(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    mut attention: Option<&mut AttentionController>,
) -> Result<(bool, bool)> {
    let Some(runtime) = worker.as_mut() else {
        return Ok((false, false));
    };
    let mut dirty = false;
    let mut projection_refresh = false;
    let mut startup_failed = false;
    let batch_started = Instant::now();
    let mut processed_messages = 0usize;
    app.begin_timeline_render_batch();
    while let Some(message) = try_recv_worker_message(runtime) {
        projection_refresh |= worker_message_requires_projection_refresh(&message);
        startup_failed |= apply_worker_message_state(runtime, attention.as_deref_mut(), &message);
        app.handle_worker_message(message)?;
        dirty = true;
        processed_messages = processed_messages.saturating_add(1);
        if processed_messages >= WORKER_MESSAGE_BATCH_LIMIT
            || batch_started.elapsed() >= WORKER_MESSAGE_BATCH_BUDGET
        {
            break;
        }
    }
    if startup_failed {
        #[cfg(not(test))]
        runtime_transition::maintain(app, worker, None)?;
        #[cfg(test)]
        shutdown_and_join_worker(worker)?;
    }
    Ok((
        dirty | app.flush_timeline_render_batch(),
        projection_refresh,
    ))
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
async fn next_worker_message(
    worker: &mut Option<WorkerRuntime>,
    session_rebind_pending: bool,
) -> Option<WorkerMessage> {
    match worker.as_mut() {
        Some(runtime) => {
            wait_for_worker_event(session_rebind_pending, runtime.worker_rx.recv()).await
        }
        None => std::future::pending().await,
    }
}

async fn wait_for_worker_event<T>(
    session_rebind_pending: bool,
    receive: impl std::future::Future<Output = Option<T>>,
) -> Option<T> {
    if session_rebind_pending {
        // SessionSwitched is the final event of the retiring worker. Its closed channel is
        // expected; keep the admission owner alive until its receipt has been consumed.
        std::future::pending().await
    } else {
        receive.await
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
        #[cfg(not(test))]
        runtime_transition::maintain(app, worker, None)?;
        #[cfg(test)]
        shutdown_and_join_worker(worker)?;
    }
    Ok(true)
}

fn apply_worker_message_state(
    runtime: &mut WorkerRuntime,
    attention: Option<&mut AttentionController>,
    message: &WorkerMessage,
) -> bool {
    for pending in &mut runtime.pending_interactions {
        if application_interaction_outcome_matches(&pending.action, message) {
            pending.reconcile_requested = true;
        }
    }
    if let Some(pending) = runtime.pending_admission.as_mut()
        && application_interaction_outcome_matches(&pending.action, message)
    {
        // Publication can request reconciliation; only an owner-validated application receipt
        // settles the original K/F. A matching UI message is not commit evidence.
        pending.reconcile_requested = true;
    }
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
    spawn_worker_fn: F,
) -> Result<bool>
where
    F: FnMut(RootConfig, &AppState) -> Result<WorkerRuntime>,
{
    #[cfg(test)]
    let mut spawn_worker_fn = spawn_worker_fn;
    #[cfg(not(test))]
    let _ = spawn_worker_fn;
    // Consume the transition's receipt and its UI completion before shutdown takes the owner.
    if worker.as_ref().is_some_and(|runtime| {
        runtime
            .pending_admission
            .as_ref()
            .is_some_and(|pending| pending.receiver.is_some() || pending.handle.is_some())
            || runtime
                .pending_interactions
                .iter()
                .any(|pending| pending.receiver.is_some() || pending.handle.is_some())
    }) || !app.take_worker_rebind_required()
    {
        return Ok(false);
    }
    retain_idle_application_admissions(app, worker)?;
    #[cfg(not(test))]
    {
        let config = app.session_runtime_config_snapshot().cloned();
        runtime_transition::maintain(app, worker, config)?;
        Ok(true)
    }
    #[cfg(test)]
    {
        app.mark_worker_not_ready();
        shutdown_and_join_worker(worker)?;
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
}

fn retain_idle_application_admissions(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
) -> Result<()> {
    let Some(runtime) = worker.as_mut() else {
        return Ok(());
    };
    for pending in runtime
        .pending_interactions
        .iter_mut()
        .chain(runtime.pending_admission.iter_mut())
    {
        if pending.receiver.is_none()
            && pending
                .handle
                .as_ref()
                .is_some_and(|handle| handle.is_finished())
        {
            wait_for_worker_thread(pending.handle.take(), Instant::now())?;
        }
    }
    let mut index = 0;
    while index < runtime.pending_interactions.len() {
        let pending = &runtime.pending_interactions[index];
        if pending.receiver.is_none() && pending.handle.is_none() {
            let mut pending = runtime.pending_interactions.remove(index);
            // Keep exact recovery material, never redispatch an old intent into a new scope.
            // Recovery remains an explicit durable recovery operation.
            pending.reconcile_requested = false;
            app.retained_application_admissions.push(pending);
        } else {
            index += 1;
        }
    }
    if runtime
        .pending_admission
        .as_ref()
        .is_some_and(|pending| pending.receiver.is_none() && pending.handle.is_none())
        && let Some(mut pending) = runtime.pending_admission.take()
    {
        pending.reconcile_requested = false;
        app.retained_application_admissions.push(pending);
    }
    Ok(())
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
            #[cfg(not(test))]
            runtime_transition::maintain(app, worker, None)?;
            #[cfg(test)]
            shutdown_and_join_worker(worker)?;
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
    #[cfg(not(test))]
    {
        let _ = spawn_worker_fn;
        if let Some(runtime) = worker.as_ref() {
            if !runtime.ready {
                app.enqueue_worker_command(command);
                return Ok(());
            }
            match runtime.worker_tx.send(command) {
                Ok(()) => return Ok(()),
                Err(error) => app.enqueue_worker_command(*error.0),
            }
        } else {
            app.enqueue_worker_command(command);
        }
        let config = app.session_runtime_config_snapshot().cloned();
        runtime_transition::maintain(app, worker, config)
    }
    #[cfg(test)]
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
                    shutdown_and_join_worker(worker)?;
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
                report_worker_unavailable(
                    app,
                    &format!("failed to restart agent worker: {error:#}"),
                )?;
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
        shutdown_and_join_worker(worker)?;
        report_worker_unavailable(app, "agent worker stopped before accepting command")
    }
}

fn report_application_admission_error(
    app: &mut AppState,
    action: &AppAction,
    error: &anyhow::Error,
) -> Result<()> {
    fail_pending_application_action(
        app,
        action,
        &format!("application command was not admitted: {error}"),
    );
    // Admission can reject an action while the worker is still waiting for its exact approval.
    // Keep that pending request and its owner intact so the user can retry the action. Prompt-like
    // actions have no durable worker event until admission returns a receipt, so every admission
    // error must release their optimistic local Preparing state; otherwise a startup race or a
    // failed application port leaves a permanent spinner with no provider request in flight.
    if matches!(
        action,
        AppAction::SubmitPrompt(_)
            | AppAction::SubmitPromptWithAttachments { .. }
            | AppAction::SubmitPlanPrompt(_)
            | AppAction::SubmitTask(_)
            | AppAction::ContinueTask { .. }
            | AppAction::InvokeInlineSkill { .. }
            | AppAction::InvokeChildSessionSkill { .. }
            | AppAction::InvokeAgentProfile { .. }
    ) && app.approval.pending.is_none()
    {
        app.clear_worker_run_state();
    }
    app.handle_worker_message(WorkerMessage::Notice(sigil_kernel::safe_persistence_text(
        &format!("application command was not admitted: {error}"),
    )))
}

fn report_worker_unavailable(app: &mut AppState, message: &str) -> Result<()> {
    // A failed send/restart can happen after the composer optimistically entered Thinking. Clear
    // that optimistic state before presenting recovery; a dead worker must never leave a stuck
    // spinner.
    app.mark_worker_not_ready();
    app.clear_worker_run_state();
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

// Scope changes retire these observations, but do not release their owners while an aborted
// async task or its already-running blocking read/ACK is still alive.
struct TuiShutdownState {
    started: Option<Instant>,
    hint_after: Duration,
    projection_observation_owners: Vec<ProjectionObservationOwner>,
}

impl Default for TuiShutdownState {
    fn default() -> Self {
        Self {
            started: None,
            hint_after: WORKER_SHUTDOWN_TIMEOUT,
            projection_observation_owners: Vec::new(),
        }
    }
}

struct ProjectionObservationOwner {
    application: Arc<application_bridge::TuiApplicationSession>,
    tasks: Vec<tokio::task::AbortHandle>,
}

impl Drop for ProjectionObservationOwner {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        // An aborted future can still own a running blocking observation. The terminal has
        // already been restored on launcher exit; keep the actual owner until it finishes.
        while self.tasks.iter().any(|task| !task.is_finished())
            || self.application.pending_observations() > 0
        {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn retain_projection_observation(
    owners: &mut Vec<ProjectionObservationOwner>,
    application: Arc<application_bridge::TuiApplicationSession>,
    task: tokio::task::AbortHandle,
) {
    if let Some(owner) = owners
        .iter_mut()
        .find(|owner| Arc::ptr_eq(&owner.application, &application))
    {
        owner.tasks.push(task);
    } else {
        owners.push(ProjectionObservationOwner {
            application,
            tasks: vec![task],
        });
    }
}

fn release_finished_projection_observations(owners: &mut Vec<ProjectionObservationOwner>) {
    owners.retain_mut(|owner| {
        owner.tasks.retain(|task| !task.is_finished());
        !owner.tasks.is_empty() || owner.application.pending_observations() > 0
    });
}

fn abort_projection_observations(owners: &[ProjectionObservationOwner]) {
    for owner in owners {
        for task in &owner.tasks {
            task.abort();
        }
    }
}

#[cfg(test)]
fn drain_projection_observations_until(
    owners: &mut Vec<ProjectionObservationOwner>,
    deadline: Instant,
) -> Result<()> {
    abort_projection_observations(owners);
    loop {
        release_finished_projection_observations(owners);
        if owners.is_empty() {
            return Ok(());
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "session observations from current or retired scopes are still running; cleanup_complete=false"
        );
        std::thread::sleep(
            Duration::from_millis(5).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

/// Requests worker shutdown and waits only within the shared UI shutdown budget.
///
/// A `JoinHandle::join` can block forever when a provider or OS call is stuck. Keep unfinished
/// owners in `worker` on timeout, and report which owned thread and worker stage exhausted the
/// budget. Terminal restoration always precedes any wait.
#[cfg(test)]
fn restore_terminal_then_join_worker(
    worker: &mut Option<WorkerRuntime>,
    deadline: Instant,
    restore: impl FnOnce() -> std::io::Result<()>,
) -> (std::io::Result<()>, Result<()>) {
    if let Some(runtime) = worker.as_ref() {
        runtime.worker_tx.begin_shutdown();
    }
    let restored = restore();
    let joined = shutdown_and_join_worker_until(worker, deadline);
    (restored, joined)
}

fn shutdown_and_join_worker(worker: &mut Option<WorkerRuntime>) -> Result<()> {
    shutdown_and_join_worker_until(worker, Instant::now() + WORKER_SHUTDOWN_TIMEOUT)
}

fn shutdown_and_join_worker_until(
    worker: &mut Option<WorkerRuntime>,
    deadline: Instant,
) -> Result<()> {
    let Some(runtime) = worker.as_mut() else {
        return Ok(());
    };
    runtime.worker_tx.begin_shutdown();
    let _ = runtime.worker_tx.send(AppState::shutdown_command());
    join_runtime_owned_thread(
        &mut runtime.join_handle,
        &runtime.worker_tx,
        deadline,
        "sigil-agent-worker",
    )?;
    if let Some(pending) = runtime.pending_admission.as_mut() {
        join_runtime_owned_thread(
            &mut pending.handle,
            &runtime.worker_tx,
            deadline,
            "application-admission",
        )?;
    }
    for pending in &mut runtime.pending_interactions {
        join_runtime_owned_thread(
            &mut pending.handle,
            &runtime.worker_tx,
            deadline,
            "application-interaction",
        )?;
    }
    #[cfg(not(test))]
    if let Err(error) = runtime.worker_rx.shutdown_until(deadline) {
        if runtime.worker_rx.handle.is_none() {
            runtime.worker_tx.record_shutdown_join_panic();
        }
        return Err(error.context(
            runtime
                .worker_tx
                .shutdown_diagnostic("sigil-tui-worker-events"),
        ));
    }
    if let Some(application) = runtime.application.as_ref() {
        while application.pending_observations() > 0 && Instant::now() < deadline {
            std::thread::sleep(
                Duration::from_millis(5).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
        anyhow::ensure!(
            application.pending_observations() == 0,
            "{}",
            runtime
                .worker_tx
                .shutdown_diagnostic("application-observation")
        );
    }
    anyhow::ensure!(
        runtime.worker_tx.cleanup_complete(),
        "{}",
        runtime.worker_tx.shutdown_diagnostic("sigil-agent-worker")
    );
    runtime.worker_tx.record_shutdown_joins_complete();
    worker.take();
    Ok(())
}

fn join_runtime_owned_thread(
    handle: &mut Option<std::thread::JoinHandle<()>>,
    worker_tx: &runner::WorkerCommandSender,
    deadline: Instant,
    component: &str,
) -> Result<()> {
    let result = wait_for_owned_thread(handle, deadline);
    if result.is_err() && handle.is_none() {
        worker_tx.record_shutdown_join_panic();
    }
    result.with_context(|| worker_tx.shutdown_diagnostic(component))
}

fn wait_for_worker_thread(
    mut handle: Option<std::thread::JoinHandle<()>>,
    deadline: Instant,
) -> Result<()> {
    let started = Instant::now();
    shutdown::drain_shutdown(
        started,
        deadline.saturating_duration_since(started),
        || {
            let mut pass = ShutdownPass::default();
            pass.observe("owned-worker-thread", poll_owned_thread(&mut handle));
            pass
        },
        |notice| tracing::warn!("{notice}"),
    )
}

fn wait_for_owned_thread(
    owned: &mut Option<std::thread::JoinHandle<()>>,
    deadline: Instant,
) -> Result<()> {
    let Some(handle) = owned.as_ref() else {
        return Ok(());
    };
    let started = Instant::now();
    let thread_name = handle
        .thread()
        .name()
        .unwrap_or("unnamed-owned-thread")
        .to_owned();
    while !handle.is_finished() && Instant::now() < deadline {
        std::thread::sleep(
            Duration::from_millis(5).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
    anyhow::ensure!(
        handle.is_finished(),
        "shutdown deadline exceeded; owned_thread={thread_name}; stage=thread-join; elapsed_ms={}; cleanup_complete=false",
        started.elapsed().as_millis()
    );
    owned.take().expect("finished owned thread remains present").join().map_err(|payload| {
        anyhow::anyhow!("owned_thread={thread_name}; stage=thread-join; elapsed_ms={}; worker panicked during shutdown: {}; cleanup_complete=false", started.elapsed().as_millis(), format_panic_payload(payload.as_ref()))
    })
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
    } else if app.has_live_preview_work() {
        Some(Duration::from_millis(32))
    } else if app.has_pending_background_tasks() {
        Some(BACKGROUND_TASK_WAKE_INTERVAL)
    } else if app.has_running_command_elapsed() {
        Some(Duration::from_secs(1))
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

struct AbortOnDropTask<T>(tokio::task::JoinHandle<T>);

impl<T> From<tokio::task::JoinHandle<T>> for AbortOnDropTask<T> {
    fn from(handle: tokio::task::JoinHandle<T>) -> Self {
        Self(handle)
    }
}

impl<T> AbortOnDropTask<T> {
    fn is_finished(&self) -> bool {
        self.0.is_finished()
    }
    fn abort(&self) {
        self.0.abort();
    }
}

impl<T> Drop for AbortOnDropTask<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl<T> std::future::Future for AbortOnDropTask<T> {
    type Output = std::result::Result<T, tokio::task::JoinError>;
    fn poll(
        self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        std::pin::Pin::new(&mut self.get_mut().0).poll(context)
    }
}

#[cfg(not(test))]
struct ProjectionRefreshTask {
    epoch: u64,
    application: Arc<application_bridge::TuiApplicationSession>,
    handle: AbortOnDropTask<
        std::result::Result<ProjectionRefreshOutcome, sigil_application::ApplicationError>,
    >,
}

#[cfg(not(test))]
struct ProjectionRefreshOutcome {
    projection: sigil_application::ApplicationProjection,
}

#[cfg(not(test))]
#[derive(Clone)]
struct PendingProjectionAck {
    epoch: u64,
    application: Arc<application_bridge::TuiApplicationSession>,
    frontier: sigil_application::ApplicationFrontier,
    event_ids: Vec<String>,
}

#[cfg(not(test))]
struct ProjectionAckTask {
    epoch: u64,
    pending: PendingProjectionAck,
    handle: AbortOnDropTask<std::result::Result<(), sigil_application::ApplicationError>>,
}

#[cfg(not(test))]
struct ProjectionDeliveryTask {
    epoch: u64,
    application: Arc<application_bridge::TuiApplicationSession>,
    handle: AbortOnDropTask<
        Result<sigil_application::AppliedDeliveryBatch, sigil_application::ApplicationError>,
    >,
}

struct WorkerRuntime {
    worker_tx: runner::WorkerCommandSender,
    application: Option<Arc<application_bridge::TuiApplicationSession>>,
    pending_admission: Option<PendingApplicationAdmission>,
    pending_interactions: Vec<PendingApplicationAdmission>,
    #[cfg(test)]
    worker_rx: std::sync::mpsc::Receiver<WorkerMessage>,
    #[cfg(not(test))]
    worker_rx: WorkerMessageInbox,
    join_handle: Option<std::thread::JoinHandle<()>>,
    ready: bool,
}

impl Drop for WorkerRuntime {
    fn drop(&mut self) {
        self.worker_tx.reserve_stop(true);
        let _ = self.worker_tx.send(AppState::shutdown_command());
        // Normal lifecycle transitions join before releasing this value. A launcher error or
        // expired exit deadline must retain the same obligation instead of detaching threads.
        for handle in std::iter::once(&mut self.join_handle)
            .chain(
                self.pending_admission
                    .iter_mut()
                    .map(|pending| &mut pending.handle),
            )
            .chain(
                self.pending_interactions
                    .iter_mut()
                    .map(|pending| &mut pending.handle),
            )
        {
            if let Some(handle) = handle.take()
                && handle.join().is_err()
            {
                self.worker_tx.record_shutdown_join_panic();
            }
        }
        #[cfg(not(test))]
        self.worker_rx.finish_shutdown();
        if let Some(application) = self.application.as_ref() {
            while application.pending_observations() > 0 {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

#[cfg(not(test))]
struct WorkerMessageInbox {
    receiver: tokio::sync::mpsc::UnboundedReceiver<WorkerMessage>,
    stopped: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

#[cfg(not(test))]
impl Drop for WorkerMessageInbox {
    fn drop(&mut self) {
        self.finish_shutdown();
    }
}

#[cfg(not(test))]
impl WorkerMessageInbox {
    fn finish_shutdown(&mut self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Release);
        if let Some(handle) = self.handle.take()
            && handle.join().is_err()
        {
            tracing::error!("worker event forwarding thread panicked during shutdown");
        }
    }
    fn empty() -> Self {
        let (_, receiver) = tokio::sync::mpsc::unbounded_channel();
        Self {
            receiver,
            stopped: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            handle: None,
        }
    }

    fn forward_from(receiver: std::sync::mpsc::Receiver<WorkerMessage>) -> Result<Self> {
        let (sender, forwarded) = tokio::sync::mpsc::unbounded_channel();
        let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let task_stopped = Arc::clone(&stopped);
        let handle = std::thread::Builder::new()
            .name("sigil-tui-worker-events".to_owned())
            .spawn(move || {
                while !task_stopped.load(std::sync::atomic::Ordering::Acquire) {
                    match receiver.recv_timeout(Duration::from_millis(25)) {
                        Ok(message) => {
                            if sender.send(message).is_err() {
                                break;
                            }
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .context("failed to start TUI worker event forwarder")?;
        Ok(Self {
            receiver: forwarded,
            stopped,
            handle: Some(handle),
        })
    }

    fn shutdown_until(&mut self, deadline: Instant) -> Result<()> {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Release);
        wait_for_owned_thread(&mut self.handle, deadline)
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
            runtime_ready: None,
            recovery_confirmation: app
                .pending_session_route_confirmation_binding()
                .map(str::to_owned),
            explicit_selection: app.pending_session_route_selection().cloned(),
        },
        app.authority_composition().cloned(),
        app.boot_cutover().cloned(),
        app.worker_session_attachment(),
    )?;
    let application = Some(
        match application_bridge::build_for_worker(
            app,
            spawned.command_tx.clone(),
            app.runtime.reasoning_effort.clone(),
            spawned.projection_owner,
        ) {
            Ok(application) => Arc::new(application),
            Err(error) => {
                let _ = spawned.command_tx.send(WorkerCommand::Shutdown);
                wait_for_worker_thread(
                    Some(spawned.join_handle),
                    Instant::now() + WORKER_SHUTDOWN_TIMEOUT,
                )?;
                return Err(error.context("failed to attach TUI application port"));
            }
        },
    );
    let mut runtime = WorkerRuntime {
        worker_tx: spawned.command_tx,
        application,
        pending_admission: None,
        pending_interactions: Vec::new(),
        worker_rx: WorkerMessageInbox::empty(),
        join_handle: Some(spawned.join_handle),
        ready: false,
    };
    // Establish the worker owner before the fallible event forwarding thread is created.
    runtime.worker_rx = WorkerMessageInbox::forward_from(spawned.message_rx)?;
    Ok(runtime)
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/main_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/launcher_lifecycle_rfc0075_tests.rs"]
mod lifecycle_rfc0075_tests;

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/launcher_user_input_recovery_tests.rs"]
mod user_input_recovery_tests;
