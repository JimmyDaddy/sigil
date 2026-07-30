use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sigil_desktop::DesktopWorkspaceManager;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_updater::{Error as UpdaterError, Update, UpdaterExt};
use thiserror::Error;
use tokio::{sync::Mutex, time::sleep};

use crate::{DesktopExitState, persist_window_state, state::DesktopAppState};

const DESKTOP_UPDATE_EVENT_NAME: &str = "sigil-update-state";
const BACKGROUND_CHECK_DELAY: Duration = Duration::from_secs(15);
const BACKGROUND_CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const UPDATE_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const UPDATE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const UPDATE_PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(120);
const MAX_UPDATE_NOTES_CHARS: usize = 4_096;
const MAX_UPDATE_BYTES: usize = 512 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DesktopUpdatePhase {
    Unsupported,
    Idle,
    Checking,
    UpToDate,
    Available,
    Downloading,
    Installing,
    ReadyToRestart,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopUpdateSnapshot {
    pub(crate) phase: DesktopUpdatePhase,
    pub(crate) channel: &'static str,
    pub(crate) current_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) published_at: Option<String>,
    pub(crate) downloaded_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error_code: Option<&'static str>,
}

impl DesktopUpdateSnapshot {
    fn initial(current_version: String, supported: bool) -> Self {
        Self {
            phase: if supported {
                DesktopUpdatePhase::Idle
            } else {
                DesktopUpdatePhase::Unsupported
            },
            channel: "beta",
            current_version,
            version: None,
            notes: None,
            published_at: None,
            downloaded_bytes: 0,
            total_bytes: None,
            error_code: None,
        }
    }
}

#[derive(Clone)]
struct PendingUpdate {
    update: Update,
}

#[derive(Clone)]
struct DesktopUpdateSession {
    snapshot: DesktopUpdateSnapshot,
    pending: Option<PendingUpdate>,
}

impl DesktopUpdateSession {
    fn new(current_version: String, supported: bool) -> Self {
        Self {
            snapshot: DesktopUpdateSnapshot::initial(current_version, supported),
            pending: None,
        }
    }

    fn begin_check(&mut self) -> Result<(), DesktopUpdateCommandError> {
        if self.snapshot.phase == DesktopUpdatePhase::Unsupported {
            return Err(DesktopUpdateCommandError::new(
                "update_not_supported",
                "Desktop updates are available only in signed macOS production builds.",
            ));
        }
        self.snapshot.phase = DesktopUpdatePhase::Checking;
        self.snapshot.downloaded_bytes = 0;
        self.snapshot.total_bytes = None;
        self.snapshot.error_code = None;
        self.pending = None;
        Ok(())
    }

    fn set_up_to_date(&mut self) {
        self.snapshot.phase = DesktopUpdatePhase::UpToDate;
        self.snapshot.version = None;
        self.snapshot.notes = None;
        self.snapshot.published_at = None;
        self.snapshot.downloaded_bytes = 0;
        self.snapshot.total_bytes = None;
        self.snapshot.error_code = None;
        self.pending = None;
    }

    fn set_available(&mut self, update: Update) {
        self.snapshot.phase = DesktopUpdatePhase::Available;
        self.snapshot.version = Some(update.version.clone());
        self.snapshot.notes = update
            .body
            .as_deref()
            .map(|notes| bounded_chars(notes, MAX_UPDATE_NOTES_CHARS));
        self.snapshot.published_at = update.date.map(|date| date.to_string());
        self.snapshot.downloaded_bytes = 0;
        self.snapshot.total_bytes = None;
        self.snapshot.error_code = None;
        self.pending = Some(PendingUpdate { update });
    }

    fn begin_download(&mut self) -> Result<Update, DesktopUpdateCommandError> {
        if self.snapshot.phase != DesktopUpdatePhase::Available {
            return Err(DesktopUpdateCommandError::new(
                "update_not_available",
                "Check for an available update before downloading it.",
            ));
        }
        let update = self
            .pending
            .as_ref()
            .map(|pending| pending.update.clone())
            .ok_or_else(|| {
                DesktopUpdateCommandError::new(
                    "update_not_available",
                    "Check for an available update before downloading it.",
                )
            })?;
        self.snapshot.phase = DesktopUpdatePhase::Downloading;
        self.snapshot.downloaded_bytes = 0;
        self.snapshot.total_bytes = None;
        self.snapshot.error_code = None;
        Ok(update)
    }

    fn record_progress(&mut self, chunk_bytes: usize, total_bytes: Option<u64>) {
        self.snapshot.downloaded_bytes = self
            .snapshot
            .downloaded_bytes
            .saturating_add(chunk_bytes as u64);
        self.snapshot.total_bytes = total_bytes;
    }

    fn begin_install(&mut self) {
        self.snapshot.phase = DesktopUpdatePhase::Installing;
        self.snapshot.error_code = None;
    }

    fn set_ready_to_restart(&mut self) {
        self.snapshot.phase = DesktopUpdatePhase::ReadyToRestart;
        self.snapshot.error_code = None;
        self.pending = None;
    }

    fn set_error(&mut self, code: &'static str) {
        self.snapshot.phase = DesktopUpdatePhase::Error;
        self.snapshot.error_code = Some(code);
        self.pending = None;
    }

    fn set_restart_error(&mut self, code: &'static str) {
        self.snapshot.error_code = Some(code);
    }
}

#[derive(Clone)]
pub(crate) struct DesktopUpdaterState {
    session: Arc<StdMutex<DesktopUpdateSession>>,
    operation: Arc<Mutex<()>>,
    background: Arc<StdMutex<Option<tauri::async_runtime::JoinHandle<()>>>>,
    last_check_path: Arc<PathBuf>,
}

impl DesktopUpdaterState {
    pub(crate) fn new(current_version: String, last_check_path: PathBuf) -> Self {
        Self {
            session: Arc::new(StdMutex::new(DesktopUpdateSession::new(
                current_version,
                runtime_updates_supported(),
            ))),
            operation: Arc::new(Mutex::new(())),
            background: Arc::new(StdMutex::new(None)),
            last_check_path: Arc::new(last_check_path),
        }
    }

    pub(crate) fn start_background_check(&self, app: AppHandle) {
        if !runtime_updates_supported() {
            return;
        }
        let state = self.clone();
        let task = tauri::async_runtime::spawn(async move {
            sleep(BACKGROUND_CHECK_DELAY).await;
            if !background_check_due(&state.last_check_path).await {
                return;
            }
            if !state.record_check_attempt().await {
                return;
            }
            let _ = state.check(&app, UpdateCheckOrigin::Background).await;
        });
        let mut background = self
            .background
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(previous) = background.replace(task) {
            previous.abort();
        }
    }

    pub(crate) fn stop_background(&self) {
        if let Some(task) = self
            .background
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
    }

    fn snapshot(&self) -> DesktopUpdateSnapshot {
        self.session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .snapshot
            .clone()
    }

    async fn check(
        &self,
        app: &AppHandle,
        origin: UpdateCheckOrigin,
    ) -> Result<DesktopUpdateSnapshot, DesktopUpdateCommandError> {
        let _operation = self.operation.try_lock().map_err(|_| {
            DesktopUpdateCommandError::new(
                "update_busy",
                "Another Desktop update operation is still running.",
            )
        })?;
        let previous = {
            let mut session = self
                .session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let previous = session.clone();
            session.begin_check()?;
            previous
        };
        self.emit(app);
        if origin == UpdateCheckOrigin::Manual {
            let _ = self.record_check_attempt().await;
        }

        let updater = match app
            .updater_builder()
            .timeout(UPDATE_REQUEST_TIMEOUT)
            .build()
        {
            Ok(updater) => updater,
            Err(error) => {
                return Err(self.fail_check(app, origin, previous, project_updater_error(&error)));
            }
        };
        match updater.check().await {
            Ok(Some(update)) => {
                if let Err(error) = validate_update_metadata(&update) {
                    return Err(self.fail(app, error));
                }
                self.session
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .set_available(update);
                self.emit(app);
                Ok(self.snapshot())
            }
            Ok(None) => {
                self.session
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .set_up_to_date();
                self.emit(app);
                Ok(self.snapshot())
            }
            Err(error) => {
                Err(self.fail_check(app, origin, previous, project_updater_error(&error)))
            }
        }
    }

    fn fail_check(
        &self,
        app: &AppHandle,
        origin: UpdateCheckOrigin,
        previous: DesktopUpdateSession,
        error: DesktopUpdateCommandError,
    ) -> DesktopUpdateCommandError {
        if origin == UpdateCheckOrigin::Background {
            *self
                .session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = previous;
            self.emit(app);
            error
        } else {
            self.fail(app, error)
        }
    }

    async fn record_check_attempt(&self) -> bool {
        let path = Arc::clone(&self.last_check_path);
        matches!(
            tokio::task::spawn_blocking(move || {
                write_last_check(&path, unix_seconds(SystemTime::now()))
            })
            .await,
            Ok(Ok(()))
        )
    }

    async fn download_and_install(
        &self,
        app: &AppHandle,
    ) -> Result<DesktopUpdateSnapshot, DesktopUpdateCommandError> {
        let _operation = self.operation.try_lock().map_err(|_| {
            DesktopUpdateCommandError::new(
                "update_busy",
                "Another Desktop update operation is still running.",
            )
        })?;
        let mut update = {
            let mut session = self
                .session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            session.begin_download()?
        };
        update.timeout = Some(UPDATE_DOWNLOAD_TIMEOUT);
        self.emit(app);

        let state = self.clone();
        let handle = app.clone();
        let mut last_emit = Instant::now()
            .checked_sub(UPDATE_PROGRESS_EMIT_INTERVAL)
            .unwrap_or_else(Instant::now);
        let download = update
            .download(
                move |chunk_bytes, total_bytes| {
                    state
                        .session
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .record_progress(chunk_bytes, total_bytes);
                    if last_emit.elapsed() >= UPDATE_PROGRESS_EMIT_INTERVAL {
                        state.emit(&handle);
                        last_emit = Instant::now();
                    }
                },
                || {},
            )
            .await;
        let bytes = match download {
            Ok(bytes) if bytes.len() <= MAX_UPDATE_BYTES => bytes,
            Ok(_) => {
                return Err(self.fail(
                    app,
                    DesktopUpdateCommandError::new(
                        "update_too_large",
                        "The signed Desktop update exceeded the allowed package size.",
                    ),
                ));
            }
            Err(error) => return Err(self.fail(app, project_updater_error(&error))),
        };

        self.session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .begin_install();
        self.emit(app);
        if let Err(error) = update.install(&bytes) {
            return Err(self.fail(app, project_install_error(&error)));
        }
        self.session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_ready_to_restart();
        self.emit(app);
        Ok(self.snapshot())
    }

    fn fail(&self, app: &AppHandle, error: DesktopUpdateCommandError) -> DesktopUpdateCommandError {
        self.session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_error(error.code);
        self.emit(app);
        error
    }

    fn mark_restart_error(&self, app: &AppHandle, code: &'static str) {
        self.session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_restart_error(code);
        self.emit(app);
    }

    fn emit(&self, app: &AppHandle) {
        let _ = app.emit(DESKTOP_UPDATE_EVENT_NAME, self.snapshot());
    }
}

#[derive(Debug, Error, Serialize)]
#[error("{message}")]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopUpdateCommandError {
    pub(crate) code: &'static str,
    pub(crate) message: &'static str,
}

impl DesktopUpdateCommandError {
    const fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }
}

#[tauri::command]
pub(crate) fn desktop_update_state(state: State<'_, DesktopUpdaterState>) -> DesktopUpdateSnapshot {
    state.snapshot()
}

#[tauri::command]
pub(crate) async fn desktop_check_for_update(
    app: AppHandle,
    state: State<'_, DesktopUpdaterState>,
) -> Result<DesktopUpdateSnapshot, DesktopUpdateCommandError> {
    state.check(&app, UpdateCheckOrigin::Manual).await
}

#[tauri::command]
pub(crate) async fn desktop_download_and_install_update(
    app: AppHandle,
    state: State<'_, DesktopUpdaterState>,
) -> Result<DesktopUpdateSnapshot, DesktopUpdateCommandError> {
    state.download_and_install(&app).await
}

#[tauri::command]
pub(crate) async fn desktop_restart_after_update(
    app: AppHandle,
    app_state: State<'_, DesktopAppState>,
    updater: State<'_, DesktopUpdaterState>,
    exit_state: State<'_, DesktopExitState>,
) -> Result<(), DesktopUpdateCommandError> {
    if updater.snapshot().phase != DesktopUpdatePhase::ReadyToRestart {
        return Err(DesktopUpdateCommandError::new(
            "update_restart_not_ready",
            "Install the signed Desktop update before restarting.",
        ));
    }
    let mut manager = app_state.manager.lock().await;
    let active_runs = match workspace_active_run_count(&mut manager).await {
        Ok(active_runs) => active_runs,
        Err(error) => {
            updater.mark_restart_error(&app, error.code);
            return Err(error);
        }
    };
    if active_runs > 0 {
        updater.mark_restart_error(&app, "update_restart_blocked");
        return Err(DesktopUpdateCommandError::new(
            "update_restart_blocked",
            "Finish or cancel active workspace tasks before restarting Sigil.",
        ));
    }
    if !exit_state.begin_cleanup() {
        updater.mark_restart_error(&app, "update_restart_busy");
        return Err(DesktopUpdateCommandError::new(
            "update_restart_busy",
            "Sigil is already closing or restarting.",
        ));
    }

    persist_window_state(&app);
    updater.stop_background();
    app_state.run_streams.stop_all().await;
    let close_results = manager.close_all().await;
    if close_results.iter().any(|(_, result)| result.is_err()) {
        exit_state.cancel_cleanup();
        updater.mark_restart_error(&app, "update_restart_cleanup_failed");
        return Err(DesktopUpdateCommandError::new(
            "update_restart_cleanup_failed",
            "One local workspace runtime could not close safely. Retry the restart.",
        ));
    }
    exit_state.allow_exit();
    app.request_restart();
    Ok(())
}

async fn workspace_active_run_count(
    manager: &mut DesktopWorkspaceManager,
) -> Result<usize, DesktopUpdateCommandError> {
    let workspaces = manager.list().map_err(|_| {
        DesktopUpdateCommandError::new(
            "update_run_state_unavailable",
            "Active workspace tasks could not be verified. Restart was not attempted.",
        )
    })?;
    let mut active_runs = 0usize;
    for workspace in workspaces {
        let client = manager.client(&workspace.id).map_err(|_| {
            DesktopUpdateCommandError::new(
                "update_run_state_unavailable",
                "Active workspace tasks could not be verified. Restart was not attempted.",
            )
        })?;
        let sessions = client.list_sessions().await.map_err(|_| {
            DesktopUpdateCommandError::new(
                "update_run_state_unavailable",
                "Active workspace tasks could not be verified. Restart was not attempted.",
            )
        })?;
        active_runs = active_runs.saturating_add(
            sessions
                .sessions
                .iter()
                .filter(|session| session.foreground_run_id.is_some())
                .count(),
        );
    }
    Ok(active_runs)
}

fn validate_update_metadata(update: &Update) -> Result<(), DesktopUpdateCommandError> {
    let url = &update.download_url;
    validate_update_asset(url.scheme(), url.host_str(), url.path(), &update.signature)
}

fn validate_update_asset(
    scheme: &str,
    host: Option<&str>,
    path: &str,
    signature: &str,
) -> Result<(), DesktopUpdateCommandError> {
    let segments = path.split('/').collect::<Vec<_>>();
    let immutable_release_asset = matches!(
        segments.as_slice(),
        ["", "JimmyDaddy", "sigil", "releases", "download", tag, file]
            if !tag.is_empty()
                && *tag != "latest"
                && !file.is_empty()
                && file.ends_with(".app.tar.gz")
    );
    if scheme != "https" || host != Some("github.com") || !immutable_release_asset {
        return Err(DesktopUpdateCommandError::new(
            "update_manifest_invalid",
            "The beta update manifest did not point to an approved HTTPS release asset.",
        ));
    }
    if signature.trim().is_empty() {
        return Err(DesktopUpdateCommandError::new(
            "update_manifest_invalid",
            "The beta update manifest did not include a valid update signature.",
        ));
    }
    Ok(())
}

fn project_updater_error(error: &UpdaterError) -> DesktopUpdateCommandError {
    match error {
        UpdaterError::Minisign(_) | UpdaterError::Base64(_) | UpdaterError::SignatureUtf8(_) => {
            DesktopUpdateCommandError::new(
                "update_signature_invalid",
                "The Desktop update signature could not be verified.",
            )
        }
        UpdaterError::Serialization(_)
        | UpdaterError::ReleaseNotFound
        | UpdaterError::TargetNotFound(_)
        | UpdaterError::TargetsNotFound(_)
        | UpdaterError::Semver(_)
        | UpdaterError::UrlParse(_)
        | UpdaterError::InsecureTransportProtocol => DesktopUpdateCommandError::new(
            "update_manifest_invalid",
            "The beta update manifest was missing, invalid, or unsafe.",
        ),
        UpdaterError::UnsupportedArch | UpdaterError::UnsupportedOs => {
            DesktopUpdateCommandError::new(
                "update_not_supported",
                "Desktop updates are not available for this platform.",
            )
        }
        _ => DesktopUpdateCommandError::new(
            "update_check_failed",
            "The beta update channel could not be reached securely.",
        ),
    }
}

fn project_install_error(error: &UpdaterError) -> DesktopUpdateCommandError {
    match error {
        UpdaterError::Minisign(_) | UpdaterError::Base64(_) | UpdaterError::SignatureUtf8(_) => {
            DesktopUpdateCommandError::new(
                "update_signature_invalid",
                "The Desktop update signature could not be verified.",
            )
        }
        UpdaterError::AuthenticationFailed => DesktopUpdateCommandError::new(
            "update_install_authorization_failed",
            "macOS did not authorize replacing the installed application.",
        ),
        _ => DesktopUpdateCommandError::new(
            "update_install_failed",
            "The signed Desktop update could not be installed.",
        ),
    }
}

fn bounded_chars(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdateCheckOrigin {
    Manual,
    Background,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DesktopUpdateLastCheck {
    checked_at_unix_seconds: u64,
}

async fn background_check_due(path: &Path) -> bool {
    let Ok(bytes) = tokio::fs::read(path).await else {
        return true;
    };
    let Ok(last_check) = serde_json::from_slice::<DesktopUpdateLastCheck>(&bytes) else {
        return true;
    };
    background_check_due_at(
        last_check.checked_at_unix_seconds,
        unix_seconds(SystemTime::now()),
    )
}

fn background_check_due_at(last_check_seconds: u64, now_seconds: u64) -> bool {
    now_seconds.saturating_sub(last_check_seconds) >= BACKGROUND_CHECK_INTERVAL.as_secs()
}

fn write_last_check(path: &Path, checked_at_unix_seconds: u64) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "desktop update check path has no parent",
        )
    })?;
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    serde_json::to_writer(
        &mut temporary,
        &DesktopUpdateLastCheck {
            checked_at_unix_seconds,
        },
    )
    .map_err(std::io::Error::other)?;
    temporary.write_all(b"\n")?;
    temporary.as_file_mut().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

const fn runtime_updates_supported() -> bool {
    cfg!(all(target_os = "macos", not(debug_assertions)))
}

#[cfg(test)]
#[path = "tests/update_tests.rs"]
mod tests;
