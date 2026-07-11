use std::path::Path;

use anyhow::Result;
use sigil_kernel::{
    CompactionConfig, CompactionRecord, CompactionThresholdStatus, JsonlSessionStore, Session,
};

use super::protocol::{CompactionTrigger, WorkerMessage};

pub(super) fn load_session(
    provider_name: &str,
    model_name: &str,
    session_log_path: &Path,
) -> Result<Session> {
    let store = JsonlSessionStore::new(session_log_path)?;
    Session::load_from_store(provider_name.to_owned(), model_name.to_owned(), store)
}

pub(super) fn load_session_with_url_capability_attachment(
    provider_name: &str,
    model_name: &str,
    session_log_path: &Path,
    previous_session: Option<&Session>,
) -> Result<Session> {
    let mut session = load_session(provider_name, model_name, session_log_path)?;
    let prior_registrar = previous_session
        .filter(|previous| previous.session_scope_id() == session.session_scope_id())
        .and_then(Session::user_url_capability_registrar);
    if let Some(registrar) = prior_registrar {
        session.try_attach_user_url_capability_registrar(registrar)?;
    } else {
        sigil_runtime::attach_session_url_capability_store(&mut session)?;
    }
    Ok(session)
}

pub(super) fn auto_compact_session(
    session: &mut Session,
    config: &CompactionConfig,
) -> Result<Option<CompactionRecord>> {
    if config.threshold_status(session.stats().last_prompt_tokens)
        != CompactionThresholdStatus::Hard
    {
        return Ok(None);
    }
    if !session.can_compact(config) {
        return Ok(None);
    }

    session.compact_now(config).map(Some)
}

pub(super) fn session_compacted_message(
    session_log_path: &Path,
    session: &Session,
    record: CompactionRecord,
    trigger: CompactionTrigger,
) -> WorkerMessage {
    WorkerMessage::SessionCompacted {
        session_log_path: session_log_path.to_path_buf(),
        provider_name: session.provider_name().to_owned(),
        model_name: session.model_name().to_owned(),
        record: Box::new(record),
        trigger,
        entries: session.entries().to_vec(),
    }
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/session_flow_tests.rs"]
mod tests;
