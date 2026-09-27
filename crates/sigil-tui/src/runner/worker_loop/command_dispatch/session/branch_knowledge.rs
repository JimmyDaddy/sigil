use super::*;
use sigil_runtime::application_branch_knowledge::{
    ApplicationBranchKnowledgeImportRequest, application_branch_knowledge_preview,
    application_branch_lineage_view, import_application_branch_knowledge,
};

pub(super) fn dispatch<P>(
    context: WorkerCommandContext<'_, P>,
    command: SessionCommand,
) -> WorkerCommandDispatchControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerCommandContext {
        root_config,
        workspace_root,
        state,
        message_tx,
        ..
    } = context;
    match command {
        SessionCommand::LoadBranchKnowledge {
            request_id,
            target_session_id,
            source_session_ref,
            source_session_id,
        } => {
            let result = (|| -> anyhow::Result<_> {
                ensure_target(state, &target_session_id)?;
                let service = local_session_lifecycle_service_for_source_for_worker(
                    root_config,
                    workspace_root,
                    &state.session.log_path,
                    state.managed_storage_writer.as_ref(),
                )
                .context("session lifecycle authority is unavailable")?;
                let preview = application_branch_knowledge_preview(
                    &service,
                    &source_session_ref,
                    &source_session_id,
                )?;
                let lineage = application_branch_lineage_view(
                    &service,
                    &source_session_ref,
                    &source_session_id,
                )?;
                Ok((preview, lineage))
            })();
            match result {
                Ok((preview, lineage)) => {
                    let _ = message_tx.send(WorkerMessage::BranchKnowledgeLoaded {
                        request_id,
                        target_session_id,
                        preview,
                        lineage,
                    });
                }
                Err(error) => {
                    let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                        request_id,
                        error: format!("{error:#}"),
                    });
                }
            }
        }
        SessionCommand::ImportBranchKnowledge {
            request_id,
            target_session_id,
            request,
        } => {
            let result = import(
                root_config,
                workspace_root,
                state,
                &target_session_id,
                &request,
            );
            match result {
                Ok((receipt, entry)) => {
                    let _ = message_tx.send(WorkerMessage::BranchKnowledgeImported {
                        request_id,
                        target_session_id,
                        receipt,
                        entry,
                    });
                }
                Err(error) => {
                    let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                        request_id,
                        error: format!("{error:#}"),
                    });
                }
            }
        }
        _ => unreachable!("branch knowledge dispatch only receives classified branch commands"),
    }
    WorkerCommandDispatchControl::Continue
}

fn ensure_target(state: &WorkerLoopState, expected: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        state
            .session
            .current
            .as_ref()
            .is_some_and(|session| session.session_scope_id() == expected),
        "branch knowledge destination session changed"
    );
    Ok(())
}

fn import(
    root_config: &RootConfig,
    workspace_root: &Path,
    state: &mut WorkerLoopState,
    target_session_id: &str,
    request: &ApplicationBranchKnowledgeImportRequest,
) -> anyhow::Result<(
    sigil_runtime::application_branch_knowledge::ApplicationBranchKnowledgeImportReceipt,
    sigil_kernel::BranchKnowledgeImportedV1,
)> {
    ensure_target(state, target_session_id)?;
    let service = local_session_lifecycle_service_for_source_for_worker(
        root_config,
        workspace_root,
        &state.session.log_path,
        state.managed_storage_writer.as_ref(),
    )
    .context("session lifecycle authority is unavailable")?;
    let session = state
        .session
        .current
        .as_mut()
        .context("branch knowledge destination is unavailable")?;
    let receipt = import_application_branch_knowledge(&service, session, request)?;
    let entry = session
        .entries()
        .iter()
        .find_map(|entry| match entry {
            sigil_kernel::SessionLogEntry::Control(
                sigil_kernel::ControlEntry::BranchKnowledgeImportedV1(entry),
            ) if entry.import_id == receipt.import_id => Some(entry.clone()),
            _ => None,
        })
        .context("branch knowledge durable receipt is missing")?;
    Ok((receipt, entry))
}
