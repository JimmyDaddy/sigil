use super::*;

pub(in crate::runner) fn preview_current_checkpoint_restore(
    session_log_path: &Path,
    current_session: Option<&Session>,
    workspace_root: &Path,
    request: &ControlledCheckpointRestoreRequest,
) -> Result<ControlledCheckpointRestorePreview, String> {
    let session = current_session.ok_or_else(|| "session state is unavailable".to_owned())?;
    let recorder = session
        .mutation_event_recorder()
        .ok_or_else(|| "checkpoint restore requires a durable session".to_owned())?;
    let records = JsonlSessionStore::read_event_records(session_log_path)
        .map_err(|error| format!("failed to read checkpoint stream: {error:#}"))?;
    sigil_kernel::preview_controlled_checkpoint_restore(
        &recorder,
        &records,
        workspace_root,
        request,
    )
    .map_err(|error| format!("failed to preview checkpoint restore: {error:#}"))
}

pub(in crate::runner) fn execute_current_checkpoint_restore(
    session_log_path: &Path,
    current_session: Option<&Session>,
    workspace_root: &Path,
    request: &ControlledCheckpointRestoreRequest,
) -> Result<ControlledCheckpointRestoreOutput, String> {
    let session = current_session.ok_or_else(|| "session state is unavailable".to_owned())?;
    let recorder = session
        .mutation_event_recorder()
        .ok_or_else(|| "checkpoint restore requires a durable session".to_owned())?;
    let records = JsonlSessionStore::read_event_records(session_log_path)
        .map_err(|error| format!("failed to read checkpoint stream: {error:#}"))?;
    sigil_kernel::execute_controlled_checkpoint_restore(
        &recorder,
        &records,
        workspace_root,
        request,
    )
    .map_err(|error| format!("failed to restore checkpoint: {error:#}"))
}

pub(in crate::runner) fn fork_current_conversation(
    session_log_path: &Path,
    current_session: Option<&Session>,
    root_config: &RootConfig,
    request: &ControlledCheckpointRestoreRequest,
) -> Result<AttachedCheckpointForkOutput, String> {
    let session = current_session.ok_or_else(|| "session state is unavailable".to_owned())?;
    let resolved_model_route = session.resolved_model_route().cloned().map_or_else(
        || {
            sigil_runtime::provider_connections::resolve_default_model_route(root_config)
                .map(|(_, route)| route)
                .map_err(|error| {
                    format!("fork with current route requires a valid saved default: {error}")
                })
        },
        Ok,
    )?;
    let provider_name = sigil_runtime::provider_connections::validate_persisted_model_route(
        root_config,
        &resolved_model_route,
    )
    .map_err(|error| format!("fork route is not available: {error}"))?;
    let parent = session_log_path
        .parent()
        .ok_or_else(|| "current session log has no parent directory".to_owned())?;
    let file_name = session_log_path
        .file_name()
        .ok_or_else(|| "current session log has no file name".to_owned())?;
    let source_session_ref = SessionRef::new_relative(file_name)
        .map_err(|error| format!("failed to bind source session: {error:#}"))?;
    let destination_path = next_fork_path(parent, session.session_scope_id(), request)?;
    let destination_attachment = Arc::new(
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            &destination_path,
        )
        .map_err(|error| format!("failed to attach conversation fork destination: {error}"))?,
    );
    let store = JsonlSessionStore::new(session_log_path)
        .map_err(|error| format!("failed to open source session store: {error:#}"))?;
    let records = JsonlSessionStore::read_event_records(session_log_path)
        .map_err(|error| format!("failed to read conversation fork stream: {error:#}"))?;
    let output = sigil_kernel::fork_conversation_at_checkpoint(
        &store,
        &records,
        &ConversationForkRequest {
            checkpoint_id: request.checkpoint_id.clone(),
            checkpoint_digest: request.checkpoint_digest.clone(),
            source_session_ref,
            destination_path,
            provider_name,
            model_name: resolved_model_route.model_ref.model_id.clone(),
            resolved_model_route: Some(resolved_model_route),
        },
    )
    .map_err(|error| format!("failed to fork conversation: {error:#}"))?;
    Ok(AttachedCheckpointForkOutput {
        output,
        attachment: destination_attachment,
    })
}

pub(in crate::runner) struct AttachedCheckpointForkOutput {
    pub(in crate::runner) output: ConversationForkOutput,
    pub(in crate::runner) attachment:
        Arc<sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease>,
}

fn next_fork_path(
    parent: &Path,
    session_id: &str,
    request: &ControlledCheckpointRestoreRequest,
) -> Result<PathBuf, String> {
    let timestamp = current_unix_time_ms();
    for attempt in 0..100_u16 {
        let suffix = stable_event_uuid(
            "sigil-tui-conversation-fork-path",
            &format!(
                "{}:{}:{}:{timestamp}:{attempt}",
                session_id, request.checkpoint_id, request.checkpoint_digest
            ),
        );
        let candidate = parent.join(format!("session-fork-{timestamp}-{}.jsonl", &suffix[..8]));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err("failed to allocate a unique conversation fork path".to_owned())
}
