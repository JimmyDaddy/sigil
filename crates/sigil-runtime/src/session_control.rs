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
            .append(&SessionLogEntry::Control(control))
            .with_context(|| format!("failed to persist {context} control"))?;
    }
    JsonlSessionStore::read_entries(session_log_path)
        .with_context(|| format!("failed to reload {context} controls"))
}
