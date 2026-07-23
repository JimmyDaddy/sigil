use std::{path::Path, process::Command, sync::mpsc};

use anyhow::{Context, Result, bail};
use sigil_kernel::{
    AgentInvocationMode, AgentInvocationSource, AgentProfileCapturedEntry, AgentResultPolicy,
    AgentRole, AgentRunAttemptId, AgentRunAttemptStartedEntry, AgentRunContextSnapshot,
    AgentThreadId, AgentThreadStartedEntry, AgentThreadStatus, AgentThreadStatusChangedEntry,
    AgentTrustState, ControlEntry, EventHandler, IsolatedWorkspaceBackend, Session,
    TaskIsolationMode, WorkspaceRootSnapshot, WriteIsolationMode, stable_workspace_id,
    task_step_owner_agent_id,
};

use crate::AgentProfileIndexContext;

use super::{
    AgentChatChildStart, AgentChatChildThread, AgentSupervisor, AgentTaskChildStart,
    AgentTaskChildThread, chat_agent_thread_id_for_call, control::append_control,
    guard::tool_scope_has_unguarded_write_capability, hash::hash_child_input,
    hash::hash_provider_capabilities, hash::hash_text, hash::short_digest,
    ids::agent_thread_id_for_task_child, ids::profile_id_for_role,
};

impl AgentSupervisor {
    pub fn begin_task_child_thread<H>(
        &self,
        session: &mut Session,
        handler: &mut H,
        start: AgentTaskChildStart,
    ) -> Result<AgentTaskChildThread>
    where
        H: EventHandler + Send + ?Sized,
    {
        validate_batch_identity_pair(start.batch_id.is_some(), start.batch_member_key.is_some())?;
        let profile_id = profile_id_for_role(start.role)?;
        let resolved_profile = self
            .registry
            .get(&profile_id)
            .with_context(|| format!("agent profile {} is not registered", profile_id.as_str()))?;
        validate_task_worktree_binding(session, &start)?;
        let snapshot = self.registry.capture_snapshot(&profile_id)?;
        let model_visible_index = self
            .registry
            .model_visible_index(&AgentProfileIndexContext::default())?;
        let thread_id = agent_thread_id_for_task_child(
            &start.task_id,
            start.plan_version,
            &start.step,
            &start.child_task_id,
        )?;
        let attempt_id = begin_attempt_id(&thread_id)?;
        let prompt_hash = hash_child_input(&start.child_input)?;
        let (provider_name, model_name) = resolved_provider_model(
            session,
            resolved_profile.profile.provider.as_deref(),
            resolved_profile.profile.model.as_deref(),
        );
        let run_context = AgentRunContextSnapshot {
            profile_snapshot_id: snapshot.snapshot_id.clone(),
            provider: provider_name.clone(),
            model: model_name.clone(),
            reasoning_effort: resolved_profile.profile.reasoning_effort.clone(),
            workspace_root: WorkspaceRootSnapshot::new(start.workspace_root.display().to_string())?,
            effective_tool_scope_hash: snapshot.resolved_tool_scope_hash.clone(),
            effective_permission_policy_hash: snapshot.resolved_permission_policy_hash.clone(),
            effective_mcp_scope_hash: snapshot.resolved_mcp_scope_hash.clone(),
            provider_capability_hash: hash_provider_capabilities(&start.provider_capabilities)?,
            model_visible_agent_index_hash: Some(model_visible_index.fingerprint),
            budget_policy_hash: self.budget.hash()?,
            provider_background_handle_ref: None,
        };

        append_control(
            session,
            handler,
            ControlEntry::AgentProfileCaptured(AgentProfileCapturedEntry {
                snapshot: snapshot.clone(),
            }),
        )?;
        append_control(
            session,
            handler,
            ControlEntry::AgentThreadStarted(AgentThreadStartedEntry {
                thread_id: thread_id.clone(),
                parent_thread_id: Some(start.parent_thread_id.clone()),
                batch_id: start.batch_id.clone(),
                batch_member_key: start.batch_member_key.clone(),
                parent_session_ref: start.parent_session_ref.clone(),
                thread_session_ref: start.child_session_ref.clone(),
                profile_id: profile_id.clone(),
                profile_snapshot_id: snapshot.snapshot_id.clone(),
                run_context,
                objective: sigil_kernel::safe_persistence_text(&start.objective),
                prompt_hash,
                invocation_mode: start.invocation_mode,
                invocation_source: start.invocation_source,
                display_name: start
                    .step
                    .display_name
                    .as_deref()
                    .map(sigil_kernel::safe_persistence_text),
                created_at_ms: None,
            }),
        )?;

        if start.role == AgentRole::SubagentWrite
            && start.invocation_mode == AgentInvocationMode::Background
            && resolved_profile.profile.result_policy == AgentResultPolicy::ForegroundMergeRequired
        {
            let reason =
                "background write-capable agents require isolated merge support".to_owned();
            append_thread_failed(session, handler, thread_id.clone(), reason.clone())?;
            bail!("background write-capable agent requires isolated merge support");
        }

        if start.role == AgentRole::SubagentWrite
            && tool_scope_has_unguarded_write_capability(&resolved_profile.profile.tool_scope)
            && start.step.effective_isolation() != TaskIsolationMode::Worktree
        {
            let reason =
                "write-capable agents require guarded changeset-only scope or path lease support"
                    .to_owned();
            append_thread_failed(session, handler, thread_id.clone(), reason.clone())?;
            bail!(
                "write-capable agent requires guarded changeset-only scope or path lease support"
            );
        }

        if let Err(reason) = self.reserve_thread(
            &thread_id,
            &attempt_id,
            &profile_id,
            &start.task_id,
            start.role,
            start.invocation_mode,
            start.parent_depth,
            None,
        ) {
            append_thread_failed(session, handler, thread_id.clone(), reason.clone())?;
            bail!("agent budget denied child session: {reason}");
        }

        append_thread_running_and_attempt(
            session,
            handler,
            &thread_id,
            &attempt_id,
            provider_name,
            model_name,
            start.invocation_mode,
            "child session started",
        )?;
        Ok(AgentTaskChildThread {
            thread_id,
            attempt_id,
            profile_id,
            parent_thread_id: start.parent_thread_id,
        })
    }

    pub fn begin_chat_child_thread<H>(
        &self,
        session: &mut Session,
        handler: &mut H,
        start: AgentChatChildStart,
    ) -> Result<AgentChatChildThread>
    where
        H: EventHandler + Send + ?Sized,
    {
        validate_batch_identity_pair(start.batch_id.is_some(), start.batch_member_key.is_some())?;
        let resolved_profile = self.registry.get(&start.profile_id).with_context(|| {
            format!(
                "agent profile {} is not registered",
                start.profile_id.as_str()
            )
        })?;
        if !resolved_profile.effective_enabled() {
            bail!("agent profile {} is disabled", start.profile_id.as_str());
        }
        if resolved_profile.trust_state != AgentTrustState::Trusted {
            bail!("agent profile {} is not trusted", start.profile_id.as_str());
        }
        let invocation_allowed = match start.invocation_source {
            AgentInvocationSource::Mention => resolved_profile.effective_user_invocation_allowed(),
            _ => resolved_profile.effective_model_invocation_allowed(),
        };
        if !invocation_allowed {
            let requirement = match start.invocation_source {
                AgentInvocationSource::Mention => "user-invocable",
                _ => "model-invocable",
            };
            bail!(
                "agent profile {} is not {}",
                start.profile_id.as_str(),
                requirement
            );
        }

        let snapshot = self.registry.capture_snapshot(&start.profile_id)?;
        let model_visible_index = self
            .registry
            .model_visible_index(&AgentProfileIndexContext::default())?;
        let thread_id = chat_agent_thread_id_for_call(&start.call_id, &start.profile_id)?;
        let attempt_id = begin_attempt_id(&thread_id)?;
        let prompt_hash = hash_text(&sigil_kernel::safe_persistence_text(&start.prompt));
        let (provider_name, model_name) = resolved_provider_model(
            session,
            resolved_profile.profile.provider.as_deref(),
            resolved_profile.profile.model.as_deref(),
        );
        let run_context = AgentRunContextSnapshot {
            profile_snapshot_id: snapshot.snapshot_id.clone(),
            provider: provider_name.clone(),
            model: model_name.clone(),
            reasoning_effort: resolved_profile.profile.reasoning_effort.clone(),
            workspace_root: WorkspaceRootSnapshot::new(start.workspace_root.display().to_string())?,
            effective_tool_scope_hash: snapshot.resolved_tool_scope_hash.clone(),
            effective_permission_policy_hash: snapshot.resolved_permission_policy_hash.clone(),
            effective_mcp_scope_hash: snapshot.resolved_mcp_scope_hash.clone(),
            provider_capability_hash: hash_provider_capabilities(&start.provider_capabilities)?,
            model_visible_agent_index_hash: Some(model_visible_index.fingerprint),
            budget_policy_hash: self.budget.hash()?,
            provider_background_handle_ref: None,
        };

        if start.delegation_admission.thread_id != thread_id
            || start.delegation_admission.profile_id != start.profile_id
            || start.delegation_admission.invocation_mode != start.invocation_mode
            || start.delegation_admission.invocation_source != start.invocation_source
            || start.delegation_admission.objective_hash
                != hash_text(&sigil_kernel::safe_persistence_text(&start.objective))
        {
            bail!("child-agent delegation admission is not bound to the requested invocation");
        }

        append_control(
            session,
            handler,
            ControlEntry::AgentProfileCaptured(AgentProfileCapturedEntry {
                snapshot: snapshot.clone(),
            }),
        )?;
        append_control(
            session,
            handler,
            ControlEntry::AgentDelegationAdmitted(start.delegation_admission.clone()),
        )?;
        append_control(
            session,
            handler,
            ControlEntry::AgentThreadStarted(AgentThreadStartedEntry {
                thread_id: thread_id.clone(),
                parent_thread_id: Some(start.parent_thread_id.clone()),
                batch_id: start.batch_id.clone(),
                batch_member_key: start.batch_member_key.clone(),
                parent_session_ref: start.parent_session_ref.clone(),
                thread_session_ref: start.child_session_ref.clone(),
                profile_id: start.profile_id.clone(),
                profile_snapshot_id: snapshot.snapshot_id.clone(),
                run_context,
                objective: sigil_kernel::safe_persistence_text(&start.objective),
                prompt_hash,
                invocation_mode: start.invocation_mode,
                invocation_source: start.invocation_source,
                display_name: start
                    .display_name_hint
                    .as_deref()
                    .map(sigil_kernel::safe_persistence_text),
                created_at_ms: None,
            }),
        )?;

        if start.role == AgentRole::SubagentWrite
            && start.invocation_mode == AgentInvocationMode::Background
            && resolved_profile.profile.result_policy == AgentResultPolicy::ForegroundMergeRequired
        {
            let reason =
                "background write-capable agents require isolated merge support".to_owned();
            append_thread_failed(session, handler, thread_id.clone(), reason.clone())?;
            bail!("background write-capable agent requires isolated merge support");
        }

        if start.role == AgentRole::SubagentWrite
            && tool_scope_has_unguarded_write_capability(&resolved_profile.profile.tool_scope)
        {
            let reason =
                "write-capable agents require guarded changeset-only scope or path lease support"
                    .to_owned();
            append_thread_failed(session, handler, thread_id.clone(), reason.clone())?;
            bail!(
                "write-capable agent requires guarded changeset-only scope or path lease support"
            );
        }

        let (mailbox_tx, mailbox_rx) =
            if matches!(start.invocation_mode, AgentInvocationMode::Background) {
                let (tx, rx) = mpsc::channel();
                (Some(tx), Some(rx))
            } else {
                (None, None)
            };

        if let Err(reason) = self.reserve_thread(
            &thread_id,
            &attempt_id,
            &start.profile_id,
            &start.budget_scope_id,
            start.role,
            start.invocation_mode,
            start.parent_depth,
            mailbox_tx,
        ) {
            append_thread_failed(session, handler, thread_id.clone(), reason.clone())?;
            bail!("agent budget denied child session: {reason}");
        }

        append_thread_running_and_attempt(
            session,
            handler,
            &thread_id,
            &attempt_id,
            provider_name,
            model_name,
            start.invocation_mode,
            "agent tool spawned child session",
        )?;
        Ok(AgentChatChildThread {
            thread_id,
            attempt_id,
            profile_id: start.profile_id,
            parent_thread_id: start.parent_thread_id,
            child_session_ref: start.child_session_ref,
            budget_scope_id: start.budget_scope_id,
            mailbox_rx,
        })
    }
}

fn validate_task_worktree_binding(session: &Session, start: &AgentTaskChildStart) -> Result<()> {
    let isolation = start.step.effective_isolation();
    let Some(isolated_workspace_id) = start.isolated_workspace_id.as_deref() else {
        if isolation == TaskIsolationMode::Worktree {
            bail!("worktree task child is missing its durable isolated workspace binding");
        }
        return Ok(());
    };
    if isolation != TaskIsolationMode::Worktree {
        bail!("non-worktree task child cannot carry an isolated workspace binding");
    }
    let projection = session.write_isolation_projection();
    let state = projection
        .isolated_workspace_states
        .get(isolated_workspace_id)
        .ok_or_else(|| anyhow::anyhow!("isolated workspace binding is not durable"))?;
    if !state.is_consistent() {
        bail!("isolated workspace binding is inconsistent");
    }
    let created = state
        .created
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("isolated workspace binding is not ready"))?;
    if created.isolation_mode != WriteIsolationMode::Worktree
        || created.backend != IsolatedWorkspaceBackend::GitWorktree
        || state
            .cleanup
            .as_ref()
            .is_some_and(|cleanup| cleanup.status.is_terminal())
    {
        bail!("isolated workspace binding is not active");
    }
    let expected_owner = task_step_owner_agent_id(
        &sigil_kernel::SequentialTaskRequest {
            task_id: start.task_id.clone(),
            parent_session_ref: start.parent_session_ref.clone(),
            objective: start.objective.clone(),
        },
        start.plan_version,
        &start.step,
    );
    if created.owner_agent_id != expected_owner {
        bail!("isolated workspace binding owner does not match the task child");
    }
    validate_task_worktree_path(
        &start.workspace_root,
        isolated_workspace_id,
        &created.parent_workspace_id,
    )
}

fn validate_task_worktree_path(
    workspace_root: &Path,
    isolated_workspace_id: &str,
    parent_workspace_id: &str,
) -> Result<()> {
    let workspace_root = std::fs::canonicalize(workspace_root)
        .context("failed to canonicalize isolated task workspace")?;
    if workspace_root.file_name().and_then(|name| name.to_str()) != Some(isolated_workspace_id) {
        bail!("isolated task workspace path does not match its durable identity");
    }
    let isolation_root = workspace_root
        .parent()
        .ok_or_else(|| anyhow::anyhow!("isolated task workspace has no owned root"))?;
    if isolation_root.file_name().and_then(|name| name.to_str()) != Some("sigil-isolated-worktrees")
    {
        bail!("isolated task workspace is outside the runtime-owned worktree root");
    }
    if !git_worktree_inventory_contains_parent(&workspace_root, parent_workspace_id)? {
        bail!("isolated task workspace parent does not match its durable binding");
    }
    Ok(())
}

fn git_worktree_inventory_contains_parent(
    isolated_workspace_root: &Path,
    parent_workspace_id: &str,
) -> Result<bool> {
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain", "-z"])
        .current_dir(isolated_workspace_root)
        .output()
        .context("failed to inspect isolated task worktree inventory")?;
    if !output.status.success() {
        bail!("failed to inspect isolated task worktree inventory");
    }
    if output.stdout.len() > 256 * 1024 {
        bail!("isolated task worktree inventory exceeded the output limit");
    }
    let inventory = std::str::from_utf8(&output.stdout)
        .context("isolated task worktree inventory was not UTF-8")?;
    for field in inventory.split('\0') {
        let Some(path) = field.strip_prefix("worktree ") else {
            continue;
        };
        if stable_workspace_id(Path::new(path)).ok().as_deref() == Some(parent_workspace_id) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn validate_batch_identity_pair(has_batch_id: bool, has_member_key: bool) -> Result<()> {
    if has_batch_id != has_member_key {
        bail!("agent batch identity requires both batch id and member key");
    }
    Ok(())
}

pub(super) fn begin_attempt_id(thread_id: &AgentThreadId) -> Result<AgentRunAttemptId> {
    AgentRunAttemptId::new(format!(
        "attempt_{}",
        short_digest(&hash_text(thread_id.as_str()))
    ))
}

fn resolved_provider_model(
    session: &Session,
    profile_provider: Option<&str>,
    profile_model: Option<&str>,
) -> (String, String) {
    (
        profile_provider
            .map(str::to_owned)
            .unwrap_or_else(|| session.provider_name().to_owned()),
        profile_model
            .map(str::to_owned)
            .unwrap_or_else(|| session.model_name().to_owned()),
    )
}

fn append_thread_failed<H>(
    session: &mut Session,
    handler: &mut H,
    thread_id: AgentThreadId,
    reason: String,
) -> Result<()>
where
    H: EventHandler + Send + ?Sized,
{
    append_control(
        session,
        handler,
        ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
            thread_id,
            status: AgentThreadStatus::Failed,
            reason: Some(reason),
            updated_at_ms: None,
        }),
    )
}

fn append_thread_running_and_attempt<H>(
    session: &mut Session,
    handler: &mut H,
    thread_id: &AgentThreadId,
    attempt_id: &AgentRunAttemptId,
    provider: String,
    model: String,
    invocation_mode: AgentInvocationMode,
    running_reason: &str,
) -> Result<()>
where
    H: EventHandler + Send + ?Sized,
{
    append_control(
        session,
        handler,
        ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
            thread_id: thread_id.clone(),
            status: AgentThreadStatus::Running,
            reason: Some(running_reason.to_owned()),
            updated_at_ms: None,
        }),
    )?;
    append_control(
        session,
        handler,
        ControlEntry::AgentRunAttemptStarted(AgentRunAttemptStartedEntry {
            thread_id: thread_id.clone(),
            attempt_id: attempt_id.clone(),
            provider,
            model,
            background: matches!(invocation_mode, AgentInvocationMode::Background),
            provider_background_handle_ref: None,
        }),
    )
}
