use std::sync::Arc;

use anyhow::Result;
use sigil_kernel::{
    ControlEntry, ImageAttachment, ImageAttachmentResolver, JsonlSessionStore, SessionLogEntry,
};

use super::{
    CapturedSessionRuntimeAttachments, load_session_with_captured_runtime_attachments,
    load_session_with_runtime_attachments,
};

struct FixedImageResolver;

impl ImageAttachmentResolver for FixedImageResolver {
    fn resolve(&self, _attachment: &ImageAttachment) -> Result<Vec<u8>> {
        Ok(vec![1])
    }
}

fn identity_log(path: &std::path::Path, provider: &str, model: &str) -> Result<()> {
    JsonlSessionStore::new(path)?.append(&SessionLogEntry::Control(ControlEntry::SessionIdentity {
        provider_name: provider.to_owned(),
        model_name: model.to_owned(),
    }))
}

#[test]
fn image_resolver_moves_across_reload_and_session_scope_changes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let first_path = temp.path().join("first.jsonl");
    let second_path = temp.path().join("second.jsonl");
    identity_log(&first_path, "provider", "model")?;
    identity_log(&second_path, "provider", "model")?;
    let mut first = load_session_with_runtime_attachments("provider", "model", &first_path, None)?;
    first.try_attach_image_attachment_resolver(Arc::new(FixedImageResolver))?;

    let same_scope =
        load_session_with_runtime_attachments("provider", "model", &first_path, Some(&first))?;
    assert!(same_scope.image_attachment_resolver().is_some());

    let other_scope =
        load_session_with_runtime_attachments("provider", "model", &second_path, Some(&first))?;
    assert_ne!(first.session_scope_id(), other_scope.session_scope_id());
    assert!(other_scope.image_attachment_resolver().is_some());
    Ok(())
}

#[test]
fn captured_runtime_attachments_survive_background_owner_handoff() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("session.jsonl");
    identity_log(&session_path, "provider", "model")?;
    let mut original =
        load_session_with_runtime_attachments("provider", "model", &session_path, None)?;
    original.try_attach_image_attachment_resolver(Arc::new(FixedImageResolver))?;
    let captured = CapturedSessionRuntimeAttachments::from_session(Some(&original));
    drop(original);

    let reloaded = load_session_with_captured_runtime_attachments(
        "provider",
        "model",
        &session_path,
        &captured,
    )?;
    assert!(reloaded.user_url_capability_registrar().is_some());
    assert!(reloaded.image_attachment_resolver().is_some());
    Ok(())
}
