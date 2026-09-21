//! Owned background lane for recovery of the command journal itself.

use super::*;
use sigil_application::{
    ControlLogRecoveryAction, ControlLogRecoveryOutcome, ControlLogRecoveryPreview,
};
use std::sync::mpsc;

#[derive(Debug, Default)]
pub(crate) struct RecoveryUiState {
    task: Option<RecoveryTask>,
    pub(crate) preview: Option<ControlLogRecoveryPreview>,
    discard_result: bool,
}

#[derive(Debug)]
struct RecoveryTask {
    receiver:
        mpsc::Receiver<Result<ControlLogRecoveryOutcome, sigil_application::ApplicationError>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for RecoveryTask {
    fn drop(&mut self) {
        // A confirmed authority mutation retains its owner through shutdown. Dropping the UI
        // receiver is not cancellation or evidence that the durable operation has stopped.
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl RecoveryUiState {
    fn invalidate_view(&mut self) {
        self.preview = None;
        self.discard_result = self.task.is_some();
    }
    pub(crate) fn poll_shutdown(&mut self) -> ShutdownPoll {
        let Some(task) = self.task.as_mut() else {
            return ShutdownPoll::Complete;
        };
        match poll_owned_thread(&mut task.handle) {
            ShutdownPoll::Pending => ShutdownPoll::Pending,
            ShutdownPoll::Failed(error) => {
                self.task.take();
                ShutdownPoll::Failed(error)
            }
            ShutdownPoll::Complete => {
                let task = self.task.take().expect("joined recovery remains owned");
                match task.receiver.try_recv() {
                    Ok(Ok(_)) => ShutdownPoll::Complete,
                    Ok(Err(error)) => ShutdownPoll::Failed(anyhow::Error::new(error)),
                    Err(_) => ShutdownPoll::Failed(anyhow::anyhow!(
                        "control log recovery result unavailable"
                    )),
                }
            }
        }
    }

    pub(crate) fn finish_shutdown(&mut self) -> Result<()> {
        if let Some(mut task) = self.task.take()
            && let Some(handle) = task.handle.take()
        {
            handle
                .join()
                .map_err(|_| anyhow::anyhow!("control log recovery worker panicked"))?;
        }
        Ok(())
    }
}

pub(super) fn replace_app_state(app: &mut AppState, mut replacement: AppState) {
    let mut recovery = std::mem::take(&mut app.control_log_recovery);
    recovery.invalidate_view();
    replacement.control_log_recovery = recovery;
    if let Some(owner) = app.runtime_transition.as_mut() {
        owner.invalidate_view();
    }
    if let Some(owner) = app.runtime_maintenance.as_mut() {
        owner.invalidate_view();
    }
    replacement.runtime_transition = app.runtime_transition.take();
    replacement.runtime_maintenance = app.runtime_maintenance.take();
    app.transfer_session_auxiliary_cleanup_to(&mut replacement);
    *app = replacement;
}

pub(super) fn start(
    app: &mut AppState,
    worker: &Option<WorkerRuntime>,
    action: ControlLogRecoveryAction,
) -> Result<()> {
    if app.control_log_recovery.task.is_some() {
        return app.handle_worker_message(WorkerMessage::Notice(
            "command history recovery is still running".to_owned(),
        ));
    }
    let application = worker
        .as_ref()
        .and_then(|worker| worker.application.clone())
        .or_else(|| {
            app.runtime_transition
                .as_ref()
                .and_then(|owner| owner.application())
        })
        .ok_or_else(|| anyhow::anyhow!("application recovery owner is unavailable"))?;
    let (sender, receiver) = mpsc::sync_channel(1);
    let handle = std::thread::Builder::new()
        .name("control-log-recovery".to_owned())
        .spawn(move || {
            let result = futures::executor::block_on(application.recover_control_log(action));
            let _ = sender.send(result);
        })?;
    app.control_log_recovery.task = Some(RecoveryTask {
        receiver,
        handle: Some(handle),
    });
    app.handle_worker_message(WorkerMessage::Notice(
        "checking command history recovery…".to_owned(),
    ))
}

pub(super) fn poll(app: &mut AppState) -> Result<bool> {
    let result = match app
        .control_log_recovery
        .task
        .as_ref()
        .map(|task| task.receiver.try_recv())
    {
        None | Some(Err(mpsc::TryRecvError::Empty)) => return Ok(false),
        Some(Err(mpsc::TryRecvError::Disconnected)) => {
            Err(sigil_application::ApplicationError::Unavailable)
        }
        Some(Ok(result)) => result,
    };
    app.control_log_recovery.finish_shutdown()?;
    if std::mem::take(&mut app.control_log_recovery.discard_result) {
        return Ok(true);
    }
    let notice = match result {
        Ok(ControlLogRecoveryOutcome::Preview(preview)) => {
            let notice = format!(
                "Command history recovery preview: generation {} → {}, preserve {} bytes ({}). Verified prefix: {} records / {} bytes; {} known commands in {} scopes, {} known unresolved. Unparsed tail: {} bytes; additional command count {}. Scope samples{}: {}. Unresolved command samples{}: {}. Confirm with /control-log confirm {}. Pending commands keep their original identity and are not retried as new effects.",
                preview.request.from_generation,
                preview.request.successor_generation,
                preview.old_byte_length,
                preview.old_content_digest.to_hex(),
                preview.impact.verified_record_count,
                preview.impact.verified_prefix_bytes,
                preview.impact.known_command_count,
                preview.impact.affected_scope_count,
                preview.impact.known_unresolved_count,
                preview.impact.unparsed_tail_bytes,
                if preview.impact.tail_command_count_unknown {
                    "unknown"
                } else {
                    "none"
                },
                if preview.impact.scopes_truncated {
                    " (truncated)"
                } else {
                    ""
                },
                preview
                    .impact
                    .affected_scopes
                    .iter()
                    .map(|scope| scope
                        .session_id
                        .as_deref()
                        .or(scope.workspace_id.as_deref())
                        .unwrap_or(&scope.scope_digest))
                    .collect::<Vec<_>>()
                    .join(", "),
                if preview.impact.commands_truncated {
                    " (truncated)"
                } else {
                    ""
                },
                preview
                    .impact
                    .unresolved_commands
                    .iter()
                    .map(|command| format!(
                        "{} ({:?}, K={})",
                        command.command_id, command.phase, command.key_digest
                    ))
                    .collect::<Vec<_>>()
                    .join(", "),
                preview.preview_digest.to_hex()
            );
            app.control_log_recovery.preview = Some(*preview);
            notice
        }
        Ok(ControlLogRecoveryOutcome::Activated(binding)) => {
            app.control_log_recovery.preview = None;
            format!(
                "Command history generation {} is active. Original history remains available for reconciliation.",
                binding.command_generation
            )
        }
        Err(error) => format!(
            "Command history recovery did not complete: {error}. Preview again to resume the same pending operation."
        ),
    };
    app.handle_worker_message(WorkerMessage::Notice(notice))?;
    Ok(true)
}
