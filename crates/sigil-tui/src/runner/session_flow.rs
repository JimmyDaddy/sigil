use std::path::Path;

use anyhow::Result;
use sigil_kernel::{JsonlSessionStore, Session};

pub(super) fn load_session(
    provider_name: &str,
    model_name: &str,
    session_log_path: &Path,
) -> Result<Session> {
    let store = JsonlSessionStore::new(session_log_path)?;
    Session::load_from_store(provider_name.to_owned(), model_name.to_owned(), store)
}

pub(super) fn load_session_with_runtime_attachments(
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
    if let Some(resolver) = previous_session.and_then(Session::image_attachment_resolver) {
        session.try_attach_image_attachment_resolver(resolver)?;
    }
    Ok(session)
}

#[cfg(test)]
#[path = "tests/session_flow_runtime_attachment_tests.rs"]
mod tests;
