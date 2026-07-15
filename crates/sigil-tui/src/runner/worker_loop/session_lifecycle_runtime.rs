use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use sigil_kernel::{
    ConversationForkOutput, ConversationForkProjection, ConversationTurnForkRequest,
    JsonlSessionStore, RootConfig, fork_conversation_at_turn,
};
use sigil_runtime::{
    LocalSessionCatalogEntry, LocalSessionCatalogState, LocalSessionLifecycleService,
    SessionDeleteOutput, SessionDeletePreview, SessionExportOutput, SessionRetentionOutput,
    SessionRetentionPolicy, SessionRetentionPreview, current_unix_time_ms, resolve_sigil_paths,
};
use uuid::Uuid;

pub(in crate::runner) fn local_session_lifecycle_service(
    root_config: &RootConfig,
    workspace_root: &Path,
) -> LocalSessionLifecycleService {
    let paths = resolve_sigil_paths(&root_config.storage, &root_config.session, workspace_root);
    LocalSessionLifecycleService::new(
        paths.workspace_id,
        paths.session_log_dir,
        paths.session_exports_root,
    )
    .with_lifecycle_journal_path(paths.session_lifecycle_journal)
}

pub(in crate::runner) fn inspect_local_session(
    service: &LocalSessionLifecycleService,
    source_path: &Path,
) -> Result<LocalSessionCatalogEntry> {
    let canonical_source = fs::canonicalize(source_path)
        .with_context(|| format!("failed to canonicalize {}", source_path.display()))?;
    let entry = service
        .catalog()?
        .entries
        .into_iter()
        .find(|entry| entry.path == canonical_source)
        .ok_or_else(|| anyhow!("source is not a cataloged direct session child"))?;
    if entry.state != LocalSessionCatalogState::Ready {
        return Err(anyhow!(
            "source session is not ready for lifecycle operations"
        ));
    }
    Ok(entry)
}

pub(in crate::runner) fn fork_local_session(
    service: &LocalSessionLifecycleService,
    source_path: &Path,
) -> Result<ConversationForkOutput> {
    let entry = inspect_local_session(service, source_path)?;
    let records = JsonlSessionStore::read_event_records(&entry.path)
        .with_context(|| format!("failed to read {}", entry.path.display()))?;
    let point = ConversationForkProjection::from_records(&records)?
        .latest()
        .cloned()
        .ok_or_else(|| anyhow!("conversation fork requires a finalized user turn"))?;
    let parent = entry
        .path
        .parent()
        .ok_or_else(|| anyhow!("source session has no parent directory"))?;
    let destination_path = allocate_fork_path(parent, current_unix_time_ms());
    let source_store = JsonlSessionStore::new(&entry.path)?;
    fork_conversation_at_turn(
        &source_store,
        &records,
        &ConversationTurnForkRequest {
            source_turn_digest: point.source_turn_digest,
            source_session_ref: entry.session_ref,
            destination_path,
            provider_name: entry
                .provider_name
                .ok_or_else(|| anyhow!("source session has no provider identity"))?,
            model_name: entry
                .model_name
                .ok_or_else(|| anyhow!("source session has no model identity"))?,
        },
    )
}

pub(in crate::runner) fn export_local_session(
    service: &LocalSessionLifecycleService,
    source_path: &Path,
) -> Result<SessionExportOutput> {
    service.export_session(source_path, None, current_unix_time_ms())
}

pub(in crate::runner) fn set_local_session_pin(
    service: &LocalSessionLifecycleService,
    source_path: &Path,
    pinned: bool,
) -> Result<LocalSessionCatalogEntry> {
    service.set_session_pin(source_path, pinned, current_unix_time_ms())?;
    inspect_local_session(service, source_path)
}

pub(in crate::runner) fn preview_local_session_delete(
    service: &LocalSessionLifecycleService,
    source_path: &Path,
    protected_paths: &[PathBuf],
) -> Result<SessionDeletePreview> {
    service.preview_delete(source_path, protected_paths)
}

pub(in crate::runner) fn apply_local_session_delete(
    service: &LocalSessionLifecycleService,
    preview: &SessionDeletePreview,
    protected_paths: &[PathBuf],
) -> Result<SessionDeleteOutput> {
    service.apply_delete(preview, protected_paths, current_unix_time_ms())
}

pub(in crate::runner) fn preview_session_retention(
    service: &LocalSessionLifecycleService,
    policy: SessionRetentionPolicy,
    protected_paths: &[PathBuf],
) -> Result<SessionRetentionPreview> {
    service.preview_retention(policy, protected_paths, current_unix_time_ms())
}

pub(in crate::runner) fn apply_session_retention(
    service: &LocalSessionLifecycleService,
    preview: &SessionRetentionPreview,
    protected_paths: &[PathBuf],
) -> Result<SessionRetentionOutput> {
    service.apply_retention(preview, protected_paths, current_unix_time_ms())
}

fn allocate_fork_path(parent: &Path, timestamp_ms: u64) -> PathBuf {
    parent.join(format!(
        "session-fork-{timestamp_ms}-{}.jsonl",
        &Uuid::new_v4().simple().to_string()[..8]
    ))
}
