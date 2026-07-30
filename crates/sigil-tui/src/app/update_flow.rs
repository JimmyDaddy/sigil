use std::{
    path::PathBuf,
    sync::mpsc::{Receiver, TryRecvError, sync_channel},
};

use sigil_updater::{
    AutomaticCheckEnvironment, BuildMetadata, CheckOptions, UpdateApplyOutcome, UpdateChannel,
    UpdateCheckOutcome, UpdateService, apply_checked_update, automatic_check_allowed,
};

use super::{AppAction, AppState, TimelineRole};

#[derive(Debug)]
pub(super) struct UpdateUiState {
    build: BuildMetadata,
    task_rx: Option<Receiver<UpdateTaskResult>>,
    startup_check_considered: bool,
}

impl Default for UpdateUiState {
    fn default() -> Self {
        Self {
            build: BuildMetadata::source("unknown", "unknown", "debug"),
            task_rx: None,
            startup_check_considered: false,
        }
    }
}

#[derive(Debug)]
enum UpdateTaskResult {
    Checked {
        explicit: bool,
        result: Result<UpdateCheckOutcome, String>,
    },
    Applied(Result<UpdateApplyOutcome, String>),
}

impl AppState {
    pub(crate) fn set_update_build_info(&mut self, build: BuildMetadata) {
        self.update_state.build = build;
    }

    pub(crate) fn update_build_info(&self) -> &BuildMetadata {
        &self.update_state.build
    }

    pub(crate) fn maybe_start_automatic_update_check(&mut self) -> bool {
        if self.is_setup_mode() || self.update_state.startup_check_considered {
            return false;
        }
        self.update_state.startup_check_considered = true;
        let Ok(current_exe) = std::env::current_exe() else {
            return false;
        };
        let install_source = self.update_state.build.install_source(&current_exe);
        if !automatic_check_allowed(
            &self.update_state.build.profile,
            install_source,
            AutomaticCheckEnvironment::current(),
        ) {
            return false;
        }
        self.start_update_check(false, false, UpdateChannel::Current)
    }

    pub(crate) fn start_update_check(
        &mut self,
        force_refresh: bool,
        explicit: bool,
        channel: UpdateChannel,
    ) -> bool {
        if self.update_state.task_rx.is_some() {
            if explicit {
                self.record_update_notice("an update operation is already running");
            }
            return explicit;
        }
        let Ok(current_exe) = std::env::current_exe() else {
            if explicit {
                self.record_update_notice("cannot locate the current Sigil executable");
            }
            return explicit;
        };
        let build = self.update_state.build.clone();
        let install_source = build.install_source(&current_exe);
        if matches!(
            install_source,
            sigil_updater::InstallSource::Source | sigil_updater::InstallSource::Unknown
        ) {
            if explicit {
                self.record_update_notice(
                    "updates are unavailable for source builds; rebuild with Cargo instead",
                );
            }
            return explicit;
        }
        let cache_file = self
            .sigil_paths
            .cache_root
            .join(sigil_updater::UPDATE_CACHE_RELATIVE_PATH);
        let (sender, receiver) = sync_channel(1);
        let spawned = std::thread::Builder::new()
            .name("sigil-update-check".to_owned())
            .spawn(move || {
                let result =
                    run_update_check(build, install_source, cache_file, force_refresh, channel);
                let _ = sender.send(UpdateTaskResult::Checked { explicit, result });
            });
        if spawned.is_err() {
            if explicit {
                self.record_update_notice("failed to start the update checker");
            }
            return explicit;
        }
        self.update_state.task_rx = Some(receiver);
        if explicit {
            self.record_update_notice("checking for Sigil updates…");
        }
        true
    }

    pub(crate) fn start_update_apply(&mut self, channel: UpdateChannel) -> bool {
        if self.update_state.task_rx.is_some() {
            self.record_update_notice("an update operation is already running");
            return true;
        }
        let Ok(current_exe) = std::env::current_exe() else {
            self.record_update_notice("cannot locate the current Sigil executable");
            return true;
        };
        let build = self.update_state.build.clone();
        let install_source = build.install_source(&current_exe);
        if matches!(
            install_source,
            sigil_updater::InstallSource::Source | sigil_updater::InstallSource::Unknown
        ) {
            self.record_update_notice(
                "updates are unavailable for source builds; rebuild with Cargo instead",
            );
            return true;
        }
        let cache_file = self
            .sigil_paths
            .cache_root
            .join(sigil_updater::UPDATE_CACHE_RELATIVE_PATH);
        let (sender, receiver) = sync_channel(1);
        let spawned = std::thread::Builder::new()
            .name("sigil-update-apply".to_owned())
            .spawn(move || {
                let result =
                    run_update_apply(build, install_source, cache_file, current_exe, channel);
                let _ = sender.send(UpdateTaskResult::Applied(result));
            });
        if spawned.is_err() {
            self.record_update_notice("failed to start the update installer");
            return true;
        }
        self.update_state.task_rx = Some(receiver);
        self.record_update_notice("checking and verifying the Sigil update…");
        true
    }

    pub(crate) fn poll_update_task(&mut self) -> bool {
        let Some(receiver) = self.update_state.task_rx.as_ref() else {
            return false;
        };
        let task_result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => {
                self.update_state.task_rx = None;
                self.record_update_notice("update worker stopped unexpectedly");
                return true;
            }
        };
        self.update_state.task_rx = None;
        match task_result {
            UpdateTaskResult::Checked { explicit, result } => {
                self.finish_update_check(explicit, result);
            }
            UpdateTaskResult::Applied(result) => self.finish_update_apply(result),
        }
        true
    }

    pub(crate) fn has_pending_update_task(&self) -> bool {
        self.update_state.task_rx.is_some()
    }

    pub(super) fn execute_update_slash_command(&mut self, arg: &str) -> Option<AppAction> {
        let normalized = arg.trim().to_ascii_lowercase();
        let mut parts = normalized.split_whitespace();
        let first = parts.next().unwrap_or("check");
        let (operation, channel_name) = match first {
            "current" | "stable" | "beta" => ("check", first),
            operation => (operation, parts.next().unwrap_or("current")),
        };
        if parts.next().is_some() {
            self.record_update_notice("usage: /update [check|refresh|apply] [current|stable|beta]");
            return None;
        }
        let channel = match channel_name {
            "current" => UpdateChannel::Current,
            "stable" => UpdateChannel::Stable,
            "beta" => UpdateChannel::Beta,
            _ => {
                self.record_update_notice(
                    "usage: /update [check|refresh|apply] [current|stable|beta]",
                );
                return None;
            }
        };
        match operation {
            "check" => Some(AppAction::CheckForUpdate {
                force_refresh: false,
                channel,
            }),
            "refresh" => Some(AppAction::CheckForUpdate {
                force_refresh: true,
                channel,
            }),
            "apply" => Some(AppAction::ApplyUpdate { channel }),
            _ => {
                self.record_update_notice(
                    "usage: /update [check|refresh|apply] [current|stable|beta]",
                );
                None
            }
        }
    }

    fn finish_update_check(&mut self, explicit: bool, result: Result<UpdateCheckOutcome, String>) {
        match result {
            Ok(outcome) => {
                let Some(candidate) = outcome.candidate.as_ref() else {
                    if explicit {
                        self.record_update_notice(format!(
                            "Sigil {} is up to date on the {} channel",
                            outcome.current_version, outcome.channel
                        ));
                    }
                    return;
                };
                let notice = if let Some(command) = outcome.managed_update_command.as_deref() {
                    format!("Sigil {} is available · run {command}", candidate.version)
                } else if candidate.security.eligible_for_apply
                    && outcome.install_source.permits_binary_replacement()
                {
                    format!(
                        "Sigil {} is available · use /update apply",
                        candidate.version
                    )
                } else {
                    format!(
                        "Sigil {} is available but install is blocked: {}",
                        candidate.version,
                        candidate
                            .security
                            .blocking_reason
                            .as_deref()
                            .unwrap_or("this installation source is not replaceable")
                    )
                };
                self.record_update_notice(notice);
            }
            Err(error) if explicit => {
                self.record_update_notice(format!("update check failed: {error}"));
            }
            Err(_) => {}
        }
    }

    fn finish_update_apply(&mut self, result: Result<UpdateApplyOutcome, String>) {
        match result {
            Ok(UpdateApplyOutcome::Installed { version }) => self.record_update_notice(format!(
                "Sigil {version} installed · restart Sigil to use it"
            )),
            Ok(UpdateApplyOutcome::ManagedExternally { command }) => self.record_update_notice(
                format!("this installation is externally managed · run {command}"),
            ),
            Err(error) => self.record_update_notice(format!("update failed: {error}")),
        }
    }

    fn record_update_notice(&mut self, notice: impl Into<String>) {
        let notice = notice.into();
        self.last_notice = Some(notice.clone());
        self.push_timeline(TimelineRole::Notice, notice.clone());
        self.push_event("update", notice);
    }
}

fn run_update_check(
    build: BuildMetadata,
    install_source: sigil_updater::InstallSource,
    cache_file: PathBuf,
    force_refresh: bool,
    channel: UpdateChannel,
) -> Result<UpdateCheckOutcome, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let service = UpdateService::github(cache_file).map_err(|error| error.to_string())?;
    runtime
        .block_on(service.check(CheckOptions {
            current_version: build.version,
            target: build.target,
            channel,
            install_source,
            force_refresh,
        }))
        .map_err(|error| error.to_string())
}

fn run_update_apply(
    build: BuildMetadata,
    install_source: sigil_updater::InstallSource,
    cache_file: PathBuf,
    current_exe: PathBuf,
    channel: UpdateChannel,
) -> Result<UpdateApplyOutcome, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let service = UpdateService::github(cache_file).map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let outcome = service
            .check(CheckOptions {
                current_version: build.version,
                target: build.target,
                channel,
                install_source,
                force_refresh: true,
            })
            .await
            .map_err(|error| error.to_string())?;
        apply_checked_update(&outcome, &current_exe)
            .await
            .map_err(|error| error.to_string())
    })
}

#[cfg(test)]
#[path = "tests/update_flow_tests.rs"]
mod tests;
