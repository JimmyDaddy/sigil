use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use sigil_kernel::{ControlEntry, JsonlSessionStore, Session, SessionLogEntry};

#[must_use]
pub fn current_unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or(0)
}

pub fn append_session_control_entries(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    controls: impl IntoIterator<Item = ControlEntry>,
    context: &str,
) -> Result<Vec<SessionLogEntry>> {
    append_session_control_entries_and_track_detached(
        session_log_path,
        current_session,
        controls,
        &mut Vec::new(),
        context,
    )
}

/// Appends controls and records each control persisted while the in-memory session is detached.
///
/// A TUI worker detaches the live [`Session`] while a run owns it. Controls accepted during that
/// interval still go directly to the append-only store; `detached_controls` lets the worker merge
/// those already-durable facts into the returned in-memory session without rereading the JSONL.
/// The sink is updated immediately after each successful append, including when a later append or
/// the final projection reload fails.
///
/// # Errors
///
/// Returns an error when the target store cannot be opened, a control cannot be appended, or the
/// detached durable projection cannot be reloaded for the caller's immediate UI update.
pub fn append_session_control_entries_and_track_detached(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    controls: impl IntoIterator<Item = ControlEntry>,
    detached_controls: &mut Vec<ControlEntry>,
    context: &str,
) -> Result<Vec<SessionLogEntry>> {
    if let Some(session) = current_session.as_mut() {
        for control in controls {
            session
                .append_control(control)
                .with_context(|| format!("failed to append {context} control"))?;
        }
        return Ok(session.entries().to_vec());
    }

    let store = JsonlSessionStore::new(session_log_path.to_path_buf())
        .with_context(|| format!("failed to open session store for {context}"))?;
    for control in controls {
        store
            .append(&SessionLogEntry::Control(control.clone()))
            .with_context(|| format!("failed to persist {context} control"))?;
        detached_controls.push(control);
    }
    JsonlSessionStore::read_entries(session_log_path)
        .with_context(|| format!("failed to reload {context} controls"))
}
