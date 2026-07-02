use super::changeset_only::task_step_owner_agent_id;
use super::*;

pub(super) fn acquire_task_write_lease<H>(
    session: &mut Session,
    handler: &mut H,
    request: &SequentialTaskRequest,
    plan_version: u32,
    step: &TaskStepSpec,
    options: &AgentRunOptions,
) -> Result<Option<WriteLeaseId>>
where
    H: EventHandler + Send,
{
    if step.effective_mode() != TaskStepMode::Write {
        return Ok(None);
    }
    match step.effective_isolation() {
        TaskIsolationMode::SequentialWorkspaceWrite => {}
        TaskIsolationMode::SharedReadOnly => {
            bail!(
                "write task step {} cannot acquire a shared-read-only write lease",
                step.step_id.as_str()
            );
        }
        TaskIsolationMode::ChangesetOnly => {
            if step.role != AgentRole::SubagentWrite {
                bail!(
                    "changeset-only write task step {} requires a subagent_write role",
                    step.step_id.as_str()
                );
            }
            return Ok(None);
        }
        TaskIsolationMode::Worktree => {
            bail!(
                "write task step {} uses unsupported isolation mode {}",
                step.step_id.as_str(),
                step.effective_isolation().as_str()
            );
        }
    }

    let workspace_id = stable_workspace_id(&options.workspace_root)?;
    let lease_seed = format!(
        "{}:{}:{}:{}",
        request.task_id.as_str(),
        plan_version,
        step.step_id.as_str(),
        workspace_id
    );
    let lease_id = WriteLeaseId::new(format!(
        "lease-{}",
        stable_event_uuid("sigil-write-lease", &lease_seed)
    ))?;
    let entry = WriteLeaseAcquired {
        lease_id: lease_id.clone(),
        workspace_id,
        owner_agent_id: task_step_owner_agent_id(request, plan_version, step),
        isolation_mode: WriteIsolationMode::SharedWorkspaceExclusive,
        scope: WriteLeaseScope::Workspace,
    };
    session
        .write_isolation_projection()
        .validate_can_acquire_shared_workspace_lease(&entry)?;
    append_task_control(session, handler, ControlEntry::WriteLeaseAcquired(entry))?;
    Ok(Some(lease_id))
}

pub(super) fn release_task_write_lease<H>(
    session: &mut Session,
    handler: &mut H,
    lease_id: Option<WriteLeaseId>,
    status: WriteLeaseReleaseStatus,
) -> Result<()>
where
    H: EventHandler + Send,
{
    let Some(lease_id) = lease_id else {
        return Ok(());
    };
    append_task_control(
        session,
        handler,
        ControlEntry::WriteLeaseReleased(WriteLeaseReleased { lease_id, status }),
    )
}

pub(super) fn write_lease_release_status_from_step_status(
    status: TaskStepStatus,
) -> WriteLeaseReleaseStatus {
    match status {
        TaskStepStatus::Cancelled => WriteLeaseReleaseStatus::Cancelled,
        TaskStepStatus::Interrupted => WriteLeaseReleaseStatus::Interrupted,
        TaskStepStatus::Failed | TaskStepStatus::Superseded => WriteLeaseReleaseStatus::Stale,
        TaskStepStatus::Pending | TaskStepStatus::Running => WriteLeaseReleaseStatus::Interrupted,
        TaskStepStatus::Completed | TaskStepStatus::Blocked => WriteLeaseReleaseStatus::Completed,
    }
}

pub(super) fn has_active_task_write_lease(
    session: &Session,
    step_options: [&AgentRunOptions; 3],
) -> Result<bool> {
    let projection = session.write_isolation_projection();
    for options in step_options {
        let workspace_id = stable_workspace_id(&options.workspace_root)?;
        if projection.has_active_write_lease(&workspace_id) {
            return Ok(true);
        }
    }
    Ok(false)
}
