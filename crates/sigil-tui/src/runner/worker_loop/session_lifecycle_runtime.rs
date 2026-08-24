use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, anyhow};
use sigil_kernel::{
    ControlEntry, ConversationForkOutput, ConversationForkProjection, ConversationTurnForkRequest,
    JsonlSessionStore, ResolvedModelRoute, RootConfig, Session, SessionLogEntry, SessionRef,
    fork_conversation_at_turn,
};
use sigil_runtime::{
    LocalSessionCatalogEntry, LocalSessionCatalogState, LocalSessionLifecycleService,
    SessionDeleteOutput, SessionDeletePreview, SessionExportOutput, SessionRetentionOutput,
    SessionRetentionPolicy, SessionRetentionPreview, current_unix_time_ms, resolve_sigil_paths,
};
use uuid::Uuid;

/// Builds the worker-owned lifecycle service and, when the boot composition is present, routes
/// its journal through the authority-declared workspace namespace. A failed managed attachment
/// is represented as `None` so callers fail closed instead of falling back to the legacy journal.
pub(in crate::runner) fn local_session_lifecycle_service_for_worker(
    root_config: &RootConfig,
    workspace_root: &Path,
    managed_writer: Option<
        &Arc<sigil_runtime::managed_storage_writer::ManagedStorageWriterAdapterV1>,
    >,
) -> Option<LocalSessionLifecycleService> {
    let paths = resolve_sigil_paths(&root_config.storage, &root_config.session, workspace_root);
    let service = LocalSessionLifecycleService::new(
        paths.workspace_id.clone(),
        paths.session_log_dir,
        paths.session_exports_root,
    )
    .with_lifecycle_journal_path(paths.session_lifecycle_journal);
    match managed_writer {
        Some(writer) => service
            .with_managed_writer(Arc::clone(writer), paths.workspace_id)
            .ok(),
        None => Some(service),
    }
}

/// Scratch-aware variant of [`local_session_lifecycle_service_for_worker`].
pub(in crate::runner) fn local_session_lifecycle_service_with_scratch_for_worker(
    root_config: &RootConfig,
    workspace_root: &Path,
    scratch_control: &sigil_tools_builtin::ScratchNamespaceControl,
    managed_writer: Option<
        &Arc<sigil_runtime::managed_storage_writer::ManagedStorageWriterAdapterV1>,
    >,
) -> Option<LocalSessionLifecycleService> {
    let paths = resolve_sigil_paths(&root_config.storage, &root_config.session, workspace_root);
    let service = LocalSessionLifecycleService::new(
        paths.workspace_id.clone(),
        paths.session_log_dir,
        paths.session_exports_root,
    )
    .with_lifecycle_journal_path(paths.session_lifecycle_journal)
    .with_scratch_cleanup(scratch_control.clone());
    match managed_writer {
        Some(writer) => service
            .with_managed_writer(Arc::clone(writer), paths.workspace_id)
            .ok(),
        None => Some(service),
    }
}

pub(in crate::runner) fn local_session_lifecycle_service_for_source(
    root_config: &RootConfig,
    workspace_root: &Path,
    source_path: &Path,
) -> Option<LocalSessionLifecycleService> {
    let paths = resolve_sigil_paths(&root_config.storage, &root_config.session, workspace_root);
    let configured_dir = fs::canonicalize(&paths.session_log_dir).ok()?;
    let source_dir = source_path
        .parent()
        .and_then(|parent| fs::canonicalize(parent).ok())?;
    if source_dir == configured_dir {
        return Some(
            LocalSessionLifecycleService::new(
                paths.workspace_id,
                paths.session_log_dir,
                paths.session_exports_root,
            )
            .with_lifecycle_journal_path(paths.session_lifecycle_journal),
        );
    }
    let managed_session_root = paths.state_root.join("managed/session-log");
    let managed_root = fs::canonicalize(&managed_session_root).ok()?;
    let relative = source_dir.strip_prefix(&managed_root).ok()?;
    (relative.components().count() == 1)
        .then(|| {
            LocalSessionLifecycleService::new(
                paths.workspace_id,
                paths.session_log_dir,
                paths.session_exports_root,
            )
            .with_lifecycle_journal_path(paths.session_lifecycle_journal)
            .with_managed_session_log_root(managed_session_root)
            .and_then(|service| {
                service.with_managed_artifact_roots(
                    paths.state_root.join("managed/artifact-store"),
                    paths.state_root.join("managed/artifact-staging"),
                )
            })
            .ok()
        })
        .flatten()
}

pub(in crate::runner) fn local_session_lifecycle_service_for_source_for_worker(
    root_config: &RootConfig,
    workspace_root: &Path,
    source_path: &Path,
    managed_writer: Option<
        &Arc<sigil_runtime::managed_storage_writer::ManagedStorageWriterAdapterV1>,
    >,
) -> Option<LocalSessionLifecycleService> {
    let service =
        local_session_lifecycle_service_for_source(root_config, workspace_root, source_path)?;
    let paths = resolve_sigil_paths(&root_config.storage, &root_config.session, workspace_root);
    match managed_writer {
        Some(writer) => service
            .with_managed_writer(Arc::clone(writer), paths.workspace_id)
            .ok(),
        None => Some(service),
    }
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
    active_session: Option<(&Path, &Session)>,
    root_config: &RootConfig,
    current_model_route: &ResolvedModelRoute,
) -> Result<AttachedConversationForkOutput> {
    let canonical_source = fs::canonicalize(source_path)
        .with_context(|| format!("failed to canonicalize {}", source_path.display()))?;
    let active_session = active_session
        .and_then(|(path, session)| fs::canonicalize(path).ok().map(|path| (path, session)))
        .filter(|(path, _)| path == &canonical_source);
    let (source_session_ref, records) = if let Some((_, session)) = active_session {
        let source_session_ref = SessionRef::new_relative(
            canonical_source
                .file_name()
                .ok_or_else(|| anyhow!("source session has no file name"))?,
        )?;
        let records = session
            .read_durable_event_records()
            .with_context(|| format!("failed to read {}", canonical_source.display()))?;
        (source_session_ref, records)
    } else {
        let entry = inspect_local_session(service, &canonical_source)?;
        let records = JsonlSessionStore::read_event_records(&entry.path)
            .with_context(|| format!("failed to read {}", entry.path.display()))?;
        (entry.session_ref, records)
    };
    let point = ConversationForkProjection::from_records(&records)?
        .latest()
        .cloned()
        .ok_or_else(|| anyhow!("conversation fork requires a finalized user turn"))?;
    let parent = canonical_source
        .parent()
        .ok_or_else(|| anyhow!("source session has no parent directory"))?;
    let destination_path = allocate_fork_path(parent, current_unix_time_ms());
    let destination_attachment = Arc::new(
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            &destination_path,
        )
        .map_err(anyhow::Error::new)?,
    );
    let source_store = JsonlSessionStore::new(&canonical_source)?;
    let resolved_model_route = source_route_for_fork(&records, root_config, current_model_route)?;
    let provider_name = sigil_runtime::provider_connections::validate_persisted_model_route(
        root_config,
        &resolved_model_route,
    )
    .map_err(anyhow::Error::new)?;
    let output = fork_conversation_at_turn(
        &source_store,
        &records,
        &ConversationTurnForkRequest {
            source_turn_digest: point.source_turn_digest,
            source_session_ref,
            destination_path,
            provider_name,
            model_name: resolved_model_route.model_ref.model_id.clone(),
            resolved_model_route: Some(resolved_model_route),
        },
    )?;
    Ok(AttachedConversationForkOutput {
        output,
        attachment: destination_attachment,
    })
}

pub(in crate::runner) struct AttachedConversationForkOutput {
    pub(in crate::runner) output: ConversationForkOutput,
    pub(in crate::runner) attachment:
        Arc<sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease>,
}

impl std::fmt::Debug for AttachedConversationForkOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AttachedConversationForkOutput")
            .field("output", &self.output)
            .field("attachment", &"<retained>")
            .finish()
    }
}

fn source_route_for_fork(
    records: &[sigil_kernel::SessionStreamRecord],
    root_config: &RootConfig,
    current_model_route: &ResolvedModelRoute,
) -> Result<ResolvedModelRoute> {
    let mut persisted = None;
    for record in records {
        if let Some(SessionLogEntry::Control(
            ControlEntry::SessionIdentity {
                resolved_model_route: Some(route),
                ..
            }
            | ControlEntry::SessionModelSelected {
                resolved_model_route: route,
                ..
            }
            | ControlEntry::SessionRouteRebound {
                resolved_model_route: route,
                ..
            },
        )) = sigil_kernel::conversation_transcript_entry_from_record(record)?
        {
            persisted = Some(route);
        }
    }
    if let Some(route) = persisted
        && sigil_runtime::provider_connections::validate_persisted_model_route(root_config, &route)
            .is_ok()
    {
        return Ok(route);
    }
    sigil_runtime::provider_connections::validate_persisted_model_route(
        root_config,
        current_model_route,
    )
    .map_err(anyhow::Error::new)
    .context("fork with current route requires the explicit current route to remain configured")?;
    Ok(current_model_route.clone())
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
