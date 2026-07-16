use std::{path::Path, sync::Arc};

use anyhow::Result;
use sigil_kernel::{
    ImageAttachmentResolver, JsonlSessionStore, Session, UserUrlCapabilityRegistrar,
};

#[derive(Clone, Default)]
pub(super) struct CapturedSessionRuntimeAttachments {
    source_session_scope_id: Option<String>,
    user_url_capability_registrar: Option<Arc<dyn UserUrlCapabilityRegistrar>>,
    image_attachment_resolver: Option<Arc<dyn ImageAttachmentResolver>>,
}

impl CapturedSessionRuntimeAttachments {
    pub(super) fn from_session(session: Option<&Session>) -> Self {
        Self {
            source_session_scope_id: session.map(|session| session.session_scope_id().to_owned()),
            user_url_capability_registrar: session.and_then(Session::user_url_capability_registrar),
            image_attachment_resolver: session.and_then(Session::image_attachment_resolver),
        }
    }
}

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
    let runtime_attachments = CapturedSessionRuntimeAttachments::from_session(previous_session);
    load_session_with_captured_runtime_attachments(
        provider_name,
        model_name,
        session_log_path,
        &runtime_attachments,
    )
}

pub(super) fn load_session_with_captured_runtime_attachments(
    provider_name: &str,
    model_name: &str,
    session_log_path: &Path,
    runtime_attachments: &CapturedSessionRuntimeAttachments,
) -> Result<Session> {
    let mut session = load_session(provider_name, model_name, session_log_path)?;
    let prior_registrar = (runtime_attachments.source_session_scope_id.as_deref()
        == Some(session.session_scope_id()))
    .then(|| runtime_attachments.user_url_capability_registrar.clone())
    .flatten();
    if let Some(registrar) = prior_registrar {
        session.try_attach_user_url_capability_registrar(registrar)?;
    } else {
        sigil_runtime::attach_session_url_capability_store(&mut session)?;
    }
    if let Some(resolver) = runtime_attachments.image_attachment_resolver.clone() {
        session.try_attach_image_attachment_resolver(resolver)?;
    }
    Ok(session)
}

#[cfg(test)]
#[path = "tests/session_flow_runtime_attachment_tests.rs"]
mod tests;
