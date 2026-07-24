use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use sigil_kernel::{
    AgentApprovalRouteEntry, AgentInvocationMode, AgentInvocationSource, AgentRole,
    AgentRouteStatus, AgentRunInput, AgentRunOptions, AgentThreadId, AgentUsageSummary,
    ApprovalHandler, ChangeSetId, ControlEntry, DEFAULT_TASK_VERIFICATION_SCOPE_HASH, EventHandler,
    EvidenceScope, ExecutionBackend, IntegrationContentClass, IntegrationEffect,
    IntegrationLaneChanged, IntegrationLaneStatus, IntegrationObservedEffect,
    IntegrationProjection, IntegrationProposalFacts, IsolatedWorkspaceBackend,
    IsolatedWorkspaceCleanupRecorded, IsolatedWorkspaceCleanupStatus, IsolatedWorkspaceCreated,
    IsolatedWorkspacePrepared, JsonlSessionStore, MultiAgentMode, MutationEventRecorder,
    ProviderCapabilities, ProviderPhysicalAttemptOutcome, ProviderRequestRejection,
    ProviderRouteCooldownError, RunEvent, SequentialTaskRequest, Session, SessionLogEntry,
    SessionRef, SessionStats, TaskChildSessionBatchCommitEnvelope,
    TaskChildSessionBatchPreparation, TaskChildSessionEntry, TaskChildSessionRunOutput,
    TaskChildSessionRunRequest, TaskChildSessionRunner, TaskChildSessionStatus, TaskId,
    TaskIntegrationRunOutput, TaskIntegrationRunRequest, TaskParticipantAttemptId,
    TaskParticipantRetryError, TaskParticipantRetryProof, TaskPlannerSessionRunOutput,
    TaskPlannerSessionRunRequest, TaskPromotionPreview, TaskPromotionPreviewInput, TaskRouteId,
    TaskRouteStatus, TaskStepId, TaskStepMode, TaskStepSpec, TaskSubagentApprovalRouteEntry,
    TaskSynthesisSessionRunOutput, TaskSynthesisSessionRunRequest, ToolApproval, ToolCall,
    ToolErrorKind, ToolExecutionStatus, ToolOperation, ToolSpec, VerificationPolicy,
    WriteIsolationMode, build_task_promotion_preview, changeset_only_child_tool_registry,
    decode_changeset_only_child_output, stable_event_uuid, stable_workspace_id,
    task_participant_child_task_id, task_participant_input_hash, task_participant_logical_run_id,
    task_step_owner_agent_id,
};
use sigil_tools_builtin::LocalExecutionBackend;

use crate::{
    agent_completion::{AgentCompletionHub, AgentCompletionRegistration},
    integration_lanes::{
        GitIntegrationPromotionPreparationRequest, GitIntegrationRunRequest, IntegrationArtifact,
        IntegrationLaneRuntimeEvent, IntegrationPromotionPreparationTarget,
        PreparedGitIntegrationPromotion, git_integration_workspace_id,
        prepare_git_integration_promotion, run_git_integration_lanes_with_events,
    },
    isolated_workspace::{
        FrozenGitWorktreeBase, GitWorktreeBaseFreezeRequest, MaterializedGitWorktree,
        freeze_git_worktree_base, materialize_git_worktree_from_frozen_base,
    },
    provider_pressure::{
        TaskProviderPressure, TaskProviderRouteConsumer, wrap_task_agent_provider,
    },
    task_completion_progress::{TaskCompletionOutcome, TaskCompletionProgressRegistration},
};

use super::{
    AgentSupervisor, AgentTaskChildStart, AgentTaskChildThread, BoxedAgent, append_control,
    hash_text,
    ids::{agent_route_id_for_call, task_route_id_for_call},
    materialize_child_agent_final_answer,
    task_discovery::{
        MAX_TASK_DISCOVERY_PROBES, TaskDiscoveryDelegate, planner_tools_with_discovery,
    },
};

/// Runtime child runner that connects kernel task orchestration to the supervisor.
pub struct AgentSupervisorTaskChildRunner {
    supervisor: AgentSupervisor,
    planner: Option<Arc<BoxedAgent>>,
    executor: Option<Arc<BoxedAgent>>,
    subagent_read: Arc<BoxedAgent>,
    subagent_write: Arc<BoxedAgent>,
    synthesis: Option<Arc<BoxedAgent>>,
    integration_verification_backend: Arc<dyn ExecutionBackend>,
    planner_discovery_max_probes: usize,
    provider_pressure: TaskProviderPressure,
}

impl AgentSupervisorTaskChildRunner {
    pub fn new(
        supervisor: AgentSupervisor,
        subagent_read: BoxedAgent,
        subagent_write: BoxedAgent,
    ) -> Self {
        let provider_pressure = supervisor.provider_pressure().clone();
        Self {
            supervisor,
            planner: None,
            executor: None,
            subagent_read: Arc::new(wrap_task_agent_provider(
                subagent_read,
                provider_pressure.clone(),
                TaskProviderRouteConsumer::SubagentRead,
            )),
            subagent_write: Arc::new(wrap_task_agent_provider(
                subagent_write,
                provider_pressure.clone(),
                TaskProviderRouteConsumer::SubagentWrite,
            )),
            synthesis: None,
            integration_verification_backend: Arc::new(LocalExecutionBackend),
            planner_discovery_max_probes: 0,
            provider_pressure,
        }
    }

    pub fn new_with_task_roles(
        supervisor: AgentSupervisor,
        planner: BoxedAgent,
        executor: BoxedAgent,
        subagent_read: BoxedAgent,
        subagent_write: BoxedAgent,
        synthesis: BoxedAgent,
    ) -> Self {
        let provider_pressure = supervisor.provider_pressure().clone();
        Self {
            supervisor,
            planner: Some(Arc::new(wrap_task_agent_provider(
                planner,
                provider_pressure.clone(),
                TaskProviderRouteConsumer::Planner,
            ))),
            executor: Some(Arc::new(wrap_task_agent_provider(
                executor,
                provider_pressure.clone(),
                TaskProviderRouteConsumer::Executor,
            ))),
            subagent_read: Arc::new(wrap_task_agent_provider(
                subagent_read,
                provider_pressure.clone(),
                TaskProviderRouteConsumer::SubagentRead,
            )),
            subagent_write: Arc::new(wrap_task_agent_provider(
                subagent_write,
                provider_pressure.clone(),
                TaskProviderRouteConsumer::SubagentWrite,
            )),
            synthesis: Some(Arc::new(wrap_task_agent_provider(
                synthesis,
                provider_pressure.clone(),
                TaskProviderRouteConsumer::Synthesis,
            ))),
            integration_verification_backend: Arc::new(LocalExecutionBackend),
            planner_discovery_max_probes: 0,
            provider_pressure,
        }
    }

    #[must_use]
    pub fn with_planner_discovery_policy(
        mut self,
        multi_agent_mode: MultiAgentMode,
        max_probes: usize,
    ) -> Self {
        self.planner_discovery_max_probes = if multi_agent_mode == MultiAgentMode::None {
            0
        } else {
            max_probes.min(MAX_TASK_DISCOVERY_PROBES)
        };
        self
    }

    /// Sets the process-local upper bound for each provider/model route's adaptive concurrency
    /// window.
    #[must_use]
    pub fn with_provider_route_concurrency_limit(self, max_concurrency: usize) -> Self {
        self.provider_pressure
            .set_max_concurrency(max_concurrency.max(1));
        self
    }

    /// Uses the same configured RFC-0003 backend for integration-lane structural checks.
    #[must_use]
    pub fn with_integration_verification_backend(
        mut self,
        backend: Arc<dyn ExecutionBackend>,
    ) -> Self {
        self.integration_verification_backend = backend;
        self
    }

    #[allow(clippy::too_many_arguments)]
    fn begin_isolated_participant<H>(
        &self,
        parent_session: &mut Session,
        handler: &mut H,
        task: &SequentialTaskRequest,
        attempt_id: TaskParticipantAttemptId,
        plan_version: u32,
        child_session_ref: SessionRef,
        child_input: AgentRunInput,
        options: AgentRunOptions,
        step: TaskStepSpec,
        agent: &BoxedAgent,
    ) -> Result<(Session, AgentTaskChildThread, TaskId)>
    where
        H: EventHandler + Send,
    {
        let child_task_id = task_participant_child_task_id(&task.task_id, &attempt_id)?;
        let child_session = build_child_session(parent_session, &child_session_ref)?;
        let child_thread = self.supervisor.begin_task_child_thread(
            parent_session,
            handler,
            AgentTaskChildStart {
                task_id: task.task_id.clone(),
                parent_thread_id: main_thread_id()?,
                parent_depth: 0,
                batch_id: None,
                batch_member_key: None,
                parent_session_ref: task.parent_session_ref.clone(),
                plan_version,
                step,
                child_task_id: child_task_id.clone(),
                child_session_ref: child_session_ref.clone(),
                child_input,
                objective: task.objective.clone(),
                workspace_root: options.workspace_root,
                provider_capabilities: child_provider_capabilities(agent),
                role: AgentRole::Planner,
                invocation_mode: AgentInvocationMode::Foreground,
                invocation_source: AgentInvocationSource::Task,
                isolated_workspace_id: None,
            },
        )?;
        Ok((child_session, child_thread, child_task_id))
    }

    async fn freeze_task_worktree_base(
        &self,
        parent_session: &Session,
        request: &TaskChildSessionRunRequest,
    ) -> Result<FrozenGitWorktreeBase> {
        let base_snapshot_id = request
            .isolated_base_snapshot_id
            .as_deref()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "worktree task child {} is missing its parent base snapshot",
                    request.step.step_id.as_str()
                )
            })?;
        let store_path = parent_session.store_path().ok_or_else(|| {
            anyhow::anyhow!(
                "worktree task child {} requires a durable parent session store",
                request.step.step_id.as_str()
            )
        })?;
        let operation_seed = format!(
            "{}:{}:{}",
            request.task.task_id.as_str(),
            request.plan_version,
            base_snapshot_id
        );
        let operation_id = format!(
            "task-worktree-overlay-{}",
            stable_event_uuid("sigil-task-worktree-overlay", &operation_seed)
        );
        let recorder =
            MutationEventRecorder::new(JsonlSessionStore::new(store_path.to_path_buf())?);
        let lease_recorder = recorder.clone();
        let lease_workspace_root = request.options.workspace_root.clone();
        let lease_operation_id = operation_id.clone();
        let _lease = tokio::task::spawn_blocking(move || {
            lease_recorder.coordinator_with_workspace_lease(
                lease_workspace_root,
                lease_operation_id,
                None,
            )
        })
        .await
        .context("worktree overlay mutation lease task failed")??;
        freeze_git_worktree_base(GitWorktreeBaseFreezeRequest {
            parent_workspace_root: request.options.workspace_root.clone(),
            base_snapshot_id: base_snapshot_id.to_owned(),
            operation_id,
            artifact_recorder: recorder,
        })
        .await
    }

    async fn freeze_integration_worktree_base(
        &self,
        parent_session: &Session,
        request: &TaskIntegrationRunRequest,
    ) -> Result<FrozenGitWorktreeBase> {
        let store_path = parent_session.store_path().ok_or_else(|| {
            anyhow::anyhow!(
                "snapshot integration plan {} requires a durable parent session store",
                request.plan.plan_id.as_str()
            )
        })?;
        let operation_seed = format!(
            "{}:{}:{}",
            request.plan.plan_id.as_str(),
            request.plan.plan_version,
            request.plan.base_snapshot_id
        );
        let operation_id = format!(
            "integration-overlay-{}",
            stable_event_uuid("sigil-integration-overlay", &operation_seed)
        );
        let recorder =
            MutationEventRecorder::new(JsonlSessionStore::new(store_path.to_path_buf())?);
        let lease_recorder = recorder.clone();
        let lease_workspace_root = request.workspace_root.clone();
        let lease_operation_id = operation_id.clone();
        let _lease = tokio::task::spawn_blocking(move || {
            lease_recorder.coordinator_with_workspace_lease(
                lease_workspace_root,
                lease_operation_id,
                None,
            )
        })
        .await
        .context("snapshot integration mutation lease task failed")??;
        freeze_git_worktree_base(GitWorktreeBaseFreezeRequest {
            parent_workspace_root: request.workspace_root.clone(),
            base_snapshot_id: request.plan.base_snapshot_id.clone(),
            operation_id,
            artifact_recorder: recorder,
        })
        .await
    }

    async fn prepare_task_worktree<H>(
        &self,
        parent_session: &mut Session,
        handler: &mut H,
        request: &TaskChildSessionRunRequest,
        frozen: &FrozenGitWorktreeBase,
    ) -> Result<MaterializedGitWorktree>
    where
        H: EventHandler + Send,
    {
        let base_snapshot_id = request
            .isolated_base_snapshot_id
            .as_deref()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "worktree task child {} is missing its parent base snapshot",
                    request.step.step_id.as_str()
                )
            })?;
        let isolated_workspace_id = task_worktree_id(request);
        if frozen.base_snapshot_id() != base_snapshot_id
            || stable_workspace_id(frozen.parent_workspace_root())?
                != stable_workspace_id(&request.options.workspace_root)?
        {
            anyhow::bail!(
                "worktree task child {} does not match the frozen parent baseline",
                request.step.step_id.as_str()
            );
        }
        let parent_workspace_id = stable_workspace_id(&request.options.workspace_root)?;
        let owner_agent_id =
            task_step_owner_agent_id(&request.task, request.plan_version, &request.step);
        let prepared = IsolatedWorkspacePrepared {
            isolated_workspace_id: isolated_workspace_id.clone(),
            parent_workspace_id: parent_workspace_id.clone(),
            owner_agent_id: owner_agent_id.clone(),
            isolation_mode: WriteIsolationMode::Worktree,
            base_snapshot_id: base_snapshot_id.to_owned(),
            backend: IsolatedWorkspaceBackend::GitWorktree,
            base_commit: Some(frozen.base_commit().to_owned()),
            overlay_digest: Some(frozen.overlay_digest().to_owned()),
            overlay_artifact_ref: Some(frozen.overlay_artifact_ref().clone()),
            overlay_content_artifact_refs: frozen.overlay_content_artifact_refs(),
            overlay_entry_count: frozen.overlay_entry_count(),
        };
        append_control(
            parent_session,
            handler,
            ControlEntry::IsolatedWorkspacePrepared(prepared),
        )?;
        let materialized =
            match materialize_git_worktree_from_frozen_base(frozen, isolated_workspace_id.clone())
                .await
            {
                Ok(materialized) => materialized,
                Err(error) => {
                    append_control(
                        parent_session,
                        handler,
                        ControlEntry::IsolatedWorkspaceCleanupRecorded(
                            IsolatedWorkspaceCleanupRecorded {
                                isolated_workspace_id,
                                status: IsolatedWorkspaceCleanupStatus::Failed,
                            },
                        ),
                    )?;
                    return Err(error);
                }
            };
        let created = IsolatedWorkspaceCreated {
            isolated_workspace_id: materialized.isolated_workspace_id().to_owned(),
            parent_workspace_id,
            owner_agent_id,
            isolation_mode: WriteIsolationMode::Worktree,
            base_snapshot_id: base_snapshot_id.to_owned(),
            backend: IsolatedWorkspaceBackend::GitWorktree,
            base_commit: Some(materialized.base_commit().to_owned()),
            overlay_digest: materialized.overlay_digest().map(str::to_owned),
            overlay_artifact_ref: materialized.overlay_artifact_ref().cloned(),
            overlay_content_artifact_refs: materialized.overlay_content_artifact_refs().to_vec(),
            overlay_entry_count: materialized.overlay_entry_count(),
            materialized_snapshot_id: Some(materialized.child_snapshot_id().to_owned()),
        };
        if let Err(error) = append_control(
            parent_session,
            handler,
            ControlEntry::IsolatedWorkspaceCreated(created),
        ) {
            let cleanup_error =
                cleanup_task_worktree(parent_session, handler, materialized).await?;
            return Err(match cleanup_error {
                Some(cleanup_error) => error.context(cleanup_error),
                None => error,
            });
        }
        Ok(materialized)
    }

    fn agent_for_step(&self, step: &TaskStepSpec) -> Result<Arc<BoxedAgent>> {
        match step.role {
            AgentRole::Planner => self
                .planner
                .clone()
                .ok_or_else(|| anyhow::anyhow!("task planner role is not configured")),
            AgentRole::Executor => self
                .executor
                .clone()
                .ok_or_else(|| anyhow::anyhow!("task executor role is not configured")),
            AgentRole::SubagentRead => Ok(Arc::clone(&self.subagent_read)),
            AgentRole::SubagentWrite => Ok(Arc::clone(&self.subagent_write)),
        }
    }

    fn preflight_parallel_task_child(
        &self,
        parent_session: &Session,
        request: TaskChildSessionRunRequest,
    ) -> Result<PreflightParallelTaskChild> {
        ParallelTaskBatchKind::for_step(&request.step)?;
        if matches!(
            request.step.effective_isolation(),
            sigil_kernel::TaskIsolationMode::ChangesetOnly
                | sigil_kernel::TaskIsolationMode::Worktree
        ) && request.isolated_base_snapshot_id.is_none()
        {
            anyhow::bail!(
                "parallel isolated task child {} is missing its parent base snapshot",
                request.step.step_id.as_str()
            );
        }
        let agent = self.agent_for_step(&request.step)?;
        let changeset_artifact_store =
            changeset_artifact_store(parent_session, &request.options.workspace_root, &request)?;
        let child_task_id =
            task_participant_child_task_id(&request.task.task_id, &request.attempt_id)?;
        let child_session_ref = request.child_session_ref.clone();
        let child_session = build_child_session(parent_session, &child_session_ref)?;
        if let Err(error) = self
            .provider_pressure
            .check(agent.provider().name(), child_session.model_name())
        {
            return Err(self.retryable_admission_error(&request, &agent, &child_session, error));
        }
        let start = AgentTaskChildStart {
            task_id: request.task.task_id.clone(),
            parent_thread_id: main_thread_id()?,
            parent_depth: 0,
            batch_id: None,
            batch_member_key: None,
            parent_session_ref: request.task.parent_session_ref.clone(),
            plan_version: request.plan_version,
            step: request.step.clone(),
            child_task_id: child_task_id.clone(),
            child_session_ref: child_session_ref.clone(),
            child_input: request.child_input.clone(),
            objective: request.task.objective.clone(),
            workspace_root: request.options.workspace_root.clone(),
            provider_capabilities: child_provider_capabilities(&agent),
            role: request.step.role,
            invocation_mode: AgentInvocationMode::JoinBeforeFinal,
            invocation_source: AgentInvocationSource::Task,
            isolated_workspace_id: None,
        };
        Ok(PreflightParallelTaskChild {
            request,
            child_task_id,
            child_session_ref,
            agent,
            start,
            child_session,
            changeset_artifact_store,
        })
    }

    fn preflight_parallel_task_batch(
        &self,
        parent_session: &Session,
        requests: &[TaskChildSessionRunRequest],
    ) -> Result<PreflightParallelTaskBatch> {
        let first = requests
            .first()
            .ok_or_else(|| anyhow::anyhow!("parallel task child batch is empty"))?;
        let task_id = first.task.task_id.clone();
        let plan_version = first.plan_version;
        let kind = ParallelTaskBatchKind::for_step(&first.step)?;
        let base_snapshot_id = first.isolated_base_snapshot_id.clone();
        let parent_workspace_id = stable_workspace_id(&first.options.workspace_root)?;
        let mut attempt_ids = BTreeSet::new();
        for request in requests {
            if request.task.task_id != task_id || request.plan_version != plan_version {
                anyhow::bail!("parallel task child batch mixes task or plan identities");
            }
            if !attempt_ids.insert(request.attempt_id.clone()) {
                anyhow::bail!(
                    "parallel task child batch contains duplicate attempt {}",
                    request.attempt_id.as_str()
                );
            }
            let member_kind = ParallelTaskBatchKind::for_step(&request.step)?;
            if member_kind != kind {
                anyhow::bail!(
                    "parallel task child batch mixes shared-read-only, changeset-only, or worktree steps"
                );
            }
            if matches!(
                kind,
                ParallelTaskBatchKind::ChangesetOnly | ParallelTaskBatchKind::Worktree
            ) && request.isolated_base_snapshot_id.as_deref() != base_snapshot_id.as_deref()
            {
                anyhow::bail!("parallel isolated task child batch mixes parent base snapshots");
            }
            if matches!(kind, ParallelTaskBatchKind::Worktree)
                && stable_workspace_id(&request.options.workspace_root)? != parent_workspace_id
            {
                anyhow::bail!("parallel worktree task child batch mixes parent workspaces");
            }
        }
        let members = requests
            .iter()
            .cloned()
            .map(|request| self.preflight_parallel_task_child(parent_session, request))
            .collect::<Result<Vec<_>>>()?;
        Ok(PreflightParallelTaskBatch {
            kind,
            task_id,
            plan_version,
            members,
        })
    }

    fn start_parallel_task_child<H>(
        &self,
        parent_session: &mut Session,
        preflight: PreflightParallelTaskChild,
        handler: &mut H,
    ) -> Result<PreparedParallelTaskChild>
    where
        H: EventHandler + Send,
    {
        let PreflightParallelTaskChild {
            request,
            child_task_id,
            child_session_ref,
            agent,
            start,
            child_session,
            changeset_artifact_store,
        } = preflight;
        let child_thread =
            self.supervisor
                .begin_task_child_thread(parent_session, handler, start)?;
        let thread_release = TaskChildThreadReleaseGuard::new(&self.supervisor, &child_thread);
        if let Err(error) = append_task_child_session(
            parent_session,
            handler,
            &request,
            &child_task_id,
            &child_session_ref,
            TaskChildSessionStatus::Started,
            None,
        ) {
            let _ = self.supervisor.record_task_child_failure(
                parent_session,
                handler,
                &child_thread,
                format!("failed to persist task child start: {error:#}"),
            );
            return Err(error);
        }
        Ok(PreparedParallelTaskChild {
            request,
            child_task_id,
            child_session_ref,
            agent,
            child_thread,
            child_session,
            worktree: None,
            parent_options: None,
            changeset_artifact_store,
            _thread_release: thread_release,
        })
    }

    async fn execute_parallel_task_child<H, A>(
        &self,
        mut prepared: PreparedParallelTaskChild,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> ExecutedParallelTaskChild
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        let mut route_handler = BufferedSupervisorTaskApprovalRouteHandler {
            inner: approval_handler,
            task_request: &prepared.request,
            child_session_ref: &prepared.child_session_ref,
            source_thread_id: &prepared.child_thread.thread_id,
            controls: Vec::new(),
        };
        let child_run = {
            let mut participant_handler = TaskParticipantEventHandler { inner: handler };
            run_task_child_agent_for_step(
                &prepared.agent,
                &mut prepared.child_session,
                prepared.request.child_input.clone(),
                prepared.request.options.clone(),
                &prepared.request.step,
                &mut participant_handler,
                &mut route_handler,
            )
            .await
        };
        let mut controls = route_handler.controls;
        let mut result = match child_run {
            Ok(output) => {
                async {
                    let mut changeset_proposal = match prepared.request.step.effective_isolation() {
                        sigil_kernel::TaskIsolationMode::ChangesetOnly => Some(
                            decode_changeset_only_child_output(&output.result.final_text)?,
                        ),
                        sigil_kernel::TaskIsolationMode::Worktree => {
                            let worktree = prepared.worktree.as_ref().ok_or_else(|| {
                                anyhow::anyhow!(
                                    "parallel worktree task child lost its materialization receipt"
                                )
                            })?;
                            worktree
                                .extract_changeset(
                                    task_worktree_changeset_id(&prepared.request)?,
                                    prepared.request.step.title.clone(),
                                    format!(
                                        "Isolated worktree changes for task step {}",
                                        prepared.request.step.step_id.as_str()
                                    ),
                                )
                                .await?
                        }
                        _ => None,
                    };
                    if let Some(proposal) = &mut changeset_proposal {
                        persist_changeset_artifact(
                            prepared.changeset_artifact_store.as_ref(),
                            &prepared.request,
                            proposal,
                        )
                        .await?;
                        bind_child_integration_facts(&prepared.child_session, proposal)?;
                    }
                    let mut outcome = output.outcome;
                    if prepared.worktree.is_some() {
                        outcome.changed_files.clear();
                    }
                    let materialized = materialize_child_agent_final_answer(
                        &mut prepared.child_session,
                        &prepared.child_session_ref,
                        &prepared.child_thread.thread_id,
                        &output.result,
                    )
                    .await?;
                    Ok(ParallelTaskChildSuccess {
                        materialized,
                        outcome,
                        usage: usage_summary_from_stats(prepared.child_session.stats()),
                        changeset_proposal,
                    })
                }
                .await
            }
            Err(error) => Err(self.retryable_child_error(
                &prepared.request,
                &prepared.agent,
                &prepared.child_session,
                error,
            )),
        };
        if let Some(worktree) = prepared.worktree.take() {
            let (cleanup, cleanup_error) = cleanup_task_worktree_detached(worktree).await;
            controls.push(cleanup);
            if let Some(cleanup_error) = cleanup_error {
                let _ = handler.handle(RunEvent::Notice(cleanup_error.clone()));
                if let Err(error) = result {
                    result = Err(error.context(cleanup_error));
                }
            }
        }
        ExecutedParallelTaskChild {
            prepared,
            controls,
            result,
        }
    }

    fn detach_prepared_task_batch<'a, H, A>(
        &'a self,
        task_id: TaskId,
        plan_version: u32,
        prepared: Vec<PreparedParallelTaskChild>,
        handler: &'a mut H,
        approval_handler: &'a mut A,
    ) -> Result<TaskChildSessionBatchPreparation<'a>>
    where
        H: EventHandler + Send + 'a,
        A: ApprovalHandler + Send + 'a,
    {
        let member_count = prepared.len();
        let completion_progress = prepared
            .iter()
            .map(|member| TaskCompletionProgressRegistration {
                step_id: member.request.step.step_id.clone(),
                title: member
                    .request
                    .step
                    .display_name
                    .clone()
                    .unwrap_or_else(|| member.request.step.title.clone()),
            })
            .collect::<Vec<_>>();
        let shared_handler = SharedTaskEventHandler {
            inner: Arc::new(Mutex::new(handler)),
        };
        let shared_approval = SharedTaskApprovalHandler {
            inner: Arc::new(Mutex::new(approval_handler)),
        };
        let registrations = prepared
            .into_iter()
            .enumerate()
            .map(|(request_index, member)| {
                let key = member.request.attempt_id.clone();
                let context = ParallelTaskCompletionContext { request_index };
                let mut member_handler = shared_handler.clone();
                let mut member_approval = shared_approval.clone();
                AgentCompletionRegistration::new(key, request_index as u64, context, async move {
                    Ok::<_, anyhow::Error>(
                        self.execute_parallel_task_child(
                            member,
                            &mut member_handler,
                            &mut member_approval,
                        )
                        .await,
                    )
                })
            })
            .collect::<Vec<_>>();
        let completion_hub = match AgentCompletionHub::from_batch(registrations) {
            Ok(completion_hub) => completion_hub,
            Err(rejection) => {
                let (error, registrations) = rejection.into_parts();
                drop(registrations);
                drop(shared_handler);
                drop(shared_approval);
                return Err(anyhow::Error::new(error).context(
                    "task completion registration violated prevalidated unique attempt identity",
                ));
            }
        };
        drop(shared_handler);
        drop(shared_approval);
        let progress_registry = self.supervisor.completion_progress().clone();
        let generation = progress_registry.begin(&task_id, plan_version, completion_progress);
        let supervisor = self.supervisor.clone();

        Ok(TaskChildSessionBatchPreparation::Detached(Box::pin(
            async move {
                let mut completed = completion_hub
                    .collect_with(|envelope| {
                        let outcome = match envelope.result.as_ref() {
                            Ok(executed) if executed.result.is_ok() => {
                                TaskCompletionOutcome::Succeeded
                            }
                            Ok(_) | Err(_) => TaskCompletionOutcome::Failed,
                        };
                        progress_registry.record_arrival(
                            generation,
                            envelope.context.request_index,
                            usize::try_from(envelope.completion_index).unwrap_or(usize::MAX),
                            outcome,
                        );
                    })
                    .await;
                completed.sort_by_key(|envelope| envelope.sequence);
                Ok(TaskChildSessionBatchCommitEnvelope::new(
                    member_count,
                    move |parent_session, handler| {
                        Ok(completed
                            .into_iter()
                            .map(|envelope| {
                                envelope.result.and_then(|executed| {
                                    Self::commit_parallel_task_child(
                                        &supervisor,
                                        parent_session,
                                        handler,
                                        executed,
                                    )
                                })
                            })
                            .collect())
                    },
                ))
            },
        )))
    }

    async fn run_parallel_worktree_batch<H, A>(
        &self,
        parent_session: &mut Session,
        requests: Vec<TaskChildSessionRunRequest>,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<Vec<Result<TaskChildSessionRunOutput>>>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        let preflight = match self.preflight_parallel_task_batch(parent_session, &requests) {
            Ok(preflight) if preflight.kind == ParallelTaskBatchKind::Worktree => preflight,
            Ok(_) => {
                return Ok(rejected_parallel_task_batch(
                    &requests,
                    anyhow::anyhow!("parallel worktree path received a non-worktree batch"),
                ));
            }
            Err(error) => return Ok(rejected_parallel_task_batch(&requests, error)),
        };
        let starts = preflight
            .members
            .iter()
            .map(|member| member.start.clone())
            .collect::<Vec<_>>();
        let reservation = match self.supervisor.reserve_task_child_batch(&starts) {
            Ok(reservation) => reservation,
            Err(error) => return Ok(rejected_parallel_task_batch(&requests, error)),
        };
        let Some(first_member) = preflight.members.first() else {
            return Ok(rejected_parallel_task_batch(
                &requests,
                anyhow::anyhow!("parallel worktree task child batch lost its preflight members"),
            ));
        };
        let frozen = match self
            .freeze_task_worktree_base(parent_session, &first_member.request)
            .await
        {
            Ok(frozen) => frozen,
            Err(error) => return Ok(rejected_parallel_task_batch(&requests, error)),
        };

        let mut materialized_members: Vec<PendingParallelWorktreeChild> =
            Vec::with_capacity(preflight.members.len());
        for mut member in preflight.members {
            let parent_options = member.request.options.clone();
            let worktree = match self
                .prepare_task_worktree(parent_session, handler, &member.request, &frozen)
                .await
            {
                Ok(worktree) => worktree,
                Err(error) => {
                    for pending in materialized_members {
                        let _ =
                            cleanup_task_worktree(parent_session, handler, pending.worktree).await;
                    }
                    return Ok(rejected_parallel_task_batch(&requests, error));
                }
            };
            member.request.options.workspace_root = worktree.workspace_root().to_path_buf();
            member.request.options.permission_context.workspace_root =
                worktree.workspace_root().to_path_buf();
            member.start.workspace_root = worktree.workspace_root().to_path_buf();
            member.start.isolated_workspace_id = Some(worktree.isolated_workspace_id().to_owned());
            materialized_members.push(PendingParallelWorktreeChild {
                preflight: member,
                worktree,
                parent_options,
            });
        }

        let mut prepared = Vec::with_capacity(materialized_members.len());
        while !materialized_members.is_empty() {
            let pending = materialized_members.remove(0);
            match self.start_parallel_task_child(parent_session, pending.preflight, handler) {
                Ok(mut started) => {
                    started.worktree = Some(pending.worktree);
                    started.parent_options = Some(pending.parent_options);
                    prepared.push(started);
                }
                Err(error) => {
                    let reason = "parallel worktree task child batch start rolled back before provider dispatch";
                    let _ = cleanup_task_worktree(parent_session, handler, pending.worktree).await;
                    for pending in materialized_members {
                        let _ =
                            cleanup_task_worktree(parent_session, handler, pending.worktree).await;
                    }
                    for started in &mut prepared {
                        let _ = append_task_child_session(
                            parent_session,
                            handler,
                            &started.request,
                            &started.child_task_id,
                            &started.child_session_ref,
                            TaskChildSessionStatus::Failed,
                            None,
                        );
                        let _ = self.supervisor.record_task_child_failure(
                            parent_session,
                            handler,
                            &started.child_thread,
                            reason.to_owned(),
                        );
                        if let Some(worktree) = started.worktree.take() {
                            let _ = cleanup_task_worktree(parent_session, handler, worktree).await;
                        }
                    }
                    return Ok(rejected_parallel_task_batch(&requests, error));
                }
            }
        }
        reservation.commit();
        let preparation = self.detach_prepared_task_batch(
            preflight.task_id,
            preflight.plan_version,
            prepared,
            handler,
            approval_handler,
        )?;
        let settled = settle_runtime_task_child_batch(preparation).await?;
        match settled {
            SettledRuntimeTaskChildBatch::Detached(commit) => {
                commit.commit(parent_session, handler)
            }
            SettledRuntimeTaskChildBatch::Fallback(_) => {
                anyhow::bail!("parallel worktree batch unexpectedly returned fallback preparation")
            }
        }
    }

    fn retryable_admission_error(
        &self,
        request: &TaskChildSessionRunRequest,
        agent: &BoxedAgent,
        child_session: &Session,
        error: anyhow::Error,
    ) -> anyhow::Error {
        if !retry_safe_step(&request.step)
            || error.downcast_ref::<ProviderRouteCooldownError>().is_none()
        {
            return error;
        }
        self.wrap_retryable_error(
            &request.attempt_id,
            &request.child_input,
            agent,
            child_session,
            TaskParticipantRetryProof::AdmissionRejectedBeforeDispatch {
                zero_output: true,
                zero_tool: true,
                zero_effect: true,
            },
            error,
        )
    }

    fn retryable_child_error(
        &self,
        request: &TaskChildSessionRunRequest,
        agent: &BoxedAgent,
        child_session: &Session,
        error: anyhow::Error,
    ) -> anyhow::Error {
        if !retry_safe_step(&request.step) {
            return error;
        }
        self.retryable_zero_effect_error(
            &request.attempt_id,
            &request.child_input,
            agent,
            child_session,
            error,
        )
    }

    fn retryable_zero_effect_error(
        &self,
        attempt_id: &TaskParticipantAttemptId,
        input: &AgentRunInput,
        agent: &BoxedAgent,
        child_session: &Session,
        error: anyhow::Error,
    ) -> anyhow::Error {
        let Ok(projection) = child_session.provider_physical_attempt_projection() else {
            return error;
        };
        let logical_run_id = task_participant_logical_run_id(attempt_id);
        let attempts = projection.attempts_for_logical_run_id(&logical_run_id);
        let [attempt] = attempts.as_slice() else {
            return error;
        };
        let Some(terminal) = attempt.terminal.as_ref() else {
            return error;
        };
        if terminal.outcome != ProviderPhysicalAttemptOutcome::ConfirmedNoModelConsumption
            || terminal.rejection != Some(ProviderRequestRejection::RateLimited)
            || !terminal.durable_output_event_ids.is_empty()
            || !terminal.durable_side_effect_event_ids.is_empty()
            || child_session.entries().iter().any(|entry| {
                matches!(
                    entry,
                    SessionLogEntry::Assistant(_) | SessionLogEntry::ToolResult(_)
                ) || matches!(
                    entry,
                    SessionLogEntry::Control(
                        ControlEntry::ToolExecution(_)
                            | ControlEntry::ToolEgress(_)
                            | ControlEntry::ChangeSetProposed(_)
                            | ControlEntry::ChangeSetApplied(_)
                            | ControlEntry::TaskPlan(_)
                    )
                )
            })
        {
            return error;
        }
        self.wrap_retryable_error(
            attempt_id,
            input,
            agent,
            child_session,
            TaskParticipantRetryProof::ProviderConfirmedNoConsumption {
                physical_attempt_id: attempt.entry.physical_attempt_id.clone(),
                request_material_fingerprint: attempt.entry.request_material_fingerprint.clone(),
                zero_output: true,
                zero_tool: true,
                zero_effect: true,
            },
            error,
        )
    }

    fn wrap_retryable_error(
        &self,
        attempt_id: &TaskParticipantAttemptId,
        input: &AgentRunInput,
        agent: &BoxedAgent,
        child_session: &Session,
        proof: TaskParticipantRetryProof,
        error: anyhow::Error,
    ) -> anyhow::Error {
        let Some((retry_after_ms, route_fingerprint)) =
            self.provider_pressure.retry_schedule_delay(
                agent.provider().name(),
                child_session.model_name(),
                attempt_id,
            )
        else {
            return error;
        };
        let Ok(input_hash) = task_participant_input_hash(input) else {
            return error;
        };
        TaskParticipantRetryError::new(retry_after_ms, route_fingerprint, input_hash, proof, error)
            .map(anyhow::Error::new)
            .unwrap_or_else(|construction_error| construction_error)
    }

    fn commit_parallel_task_child<H>(
        supervisor: &AgentSupervisor,
        parent_session: &mut Session,
        handler: &mut H,
        executed: ExecutedParallelTaskChild,
    ) -> Result<TaskChildSessionRunOutput>
    where
        H: EventHandler + Send + ?Sized,
    {
        let ExecutedParallelTaskChild {
            prepared,
            controls,
            result,
        } = executed;
        for control in controls {
            parent_session.append_control(control)?;
        }
        let success = match result {
            Ok(success) => success,
            Err(error) => {
                append_task_child_session(
                    parent_session,
                    handler,
                    &prepared.request,
                    &prepared.child_task_id,
                    &prepared.child_session_ref,
                    TaskChildSessionStatus::Failed,
                    None,
                )?;
                supervisor.record_task_child_failure(
                    parent_session,
                    handler,
                    &prepared.child_thread,
                    format!("{error:#}"),
                )?;
                return Err(error);
            }
        };
        let isolated_parent_snapshot_id =
            if let Some(base_snapshot_id) = prepared.request.isolated_base_snapshot_id.as_deref() {
                match sigil_kernel::validate_isolated_parent_snapshot_unchanged_for_task(
                    parent_session,
                    &prepared.request.task,
                    prepared.request.plan_version,
                    &prepared.request.step,
                    prepared
                        .parent_options
                        .as_ref()
                        .unwrap_or(&prepared.request.options),
                    base_snapshot_id,
                ) {
                    Ok(snapshot_id) => Some(snapshot_id),
                    Err(error) => {
                        append_task_child_session(
                            parent_session,
                            handler,
                            &prepared.request,
                            &prepared.child_task_id,
                            &prepared.child_session_ref,
                            TaskChildSessionStatus::Failed,
                            None,
                        )?;
                        supervisor.record_task_child_failure(
                            parent_session,
                            handler,
                            &prepared.child_thread,
                            format!("{error:#}"),
                        )?;
                        return Err(error);
                    }
                }
            } else {
                None
            };
        let budget_warning = supervisor
            .validate_usage_budget(&prepared.request.task.task_id, &success.usage)
            .err()
            .map(|error| format!("{error:#}"));
        let status =
            task_child_status_from_outcome(&success.materialized.final_text, &success.outcome);
        append_task_child_session(
            parent_session,
            handler,
            &prepared.request,
            &prepared.child_task_id,
            &prepared.child_session_ref,
            status,
            Some(hash_text(&success.materialized.final_text)),
        )?;
        supervisor.record_task_child_result(
            parent_session,
            handler,
            &prepared.child_thread,
            prepared.child_session_ref.clone(),
            status,
            &success.materialized,
            &success.outcome,
            Some(success.usage),
        )?;
        if let Some(warning) = budget_warning {
            let _ = handler.handle(RunEvent::Notice(format!(
                "agent budget warning after child completion: {warning}"
            )));
        }
        Ok(TaskChildSessionRunOutput {
            attempt_id: prepared.request.attempt_id,
            final_text: success.materialized.final_text,
            outcome: success.outcome,
            child_session_ref: prepared.child_session_ref,
            final_answer_ref: success.materialized.final_answer_ref,
            artifact_refs: success.materialized.extra_artifacts,
            changeset_proposal: success.changeset_proposal,
            isolated_parent_snapshot_id,
        })
    }
}

struct PreflightParallelTaskChild {
    request: TaskChildSessionRunRequest,
    child_task_id: TaskId,
    child_session_ref: SessionRef,
    agent: Arc<BoxedAgent>,
    start: AgentTaskChildStart,
    child_session: Session,
    changeset_artifact_store: Option<ChangesetArtifactStore>,
}

struct PreflightParallelTaskBatch {
    kind: ParallelTaskBatchKind,
    task_id: TaskId,
    plan_version: u32,
    members: Vec<PreflightParallelTaskChild>,
}

struct PendingParallelWorktreeChild {
    preflight: PreflightParallelTaskChild,
    worktree: MaterializedGitWorktree,
    parent_options: AgentRunOptions,
}

#[derive(Clone)]
struct ChangesetArtifactStore {
    recorder: MutationEventRecorder,
    workspace_id: String,
}

fn changeset_artifact_store(
    parent_session: &Session,
    parent_workspace_root: &Path,
    request: &TaskChildSessionRunRequest,
) -> Result<Option<ChangesetArtifactStore>> {
    if !matches!(
        request.step.effective_isolation(),
        sigil_kernel::TaskIsolationMode::ChangesetOnly | sigil_kernel::TaskIsolationMode::Worktree
    ) {
        return Ok(None);
    }
    let Some(recorder) = parent_session.mutation_event_recorder() else {
        return Ok(None);
    };
    Ok(Some(ChangesetArtifactStore {
        recorder,
        workspace_id: stable_workspace_id(parent_workspace_root)?,
    }))
}

async fn persist_changeset_artifact(
    store: Option<&ChangesetArtifactStore>,
    request: &TaskChildSessionRunRequest,
    proposal: &mut sigil_kernel::TaskChildChangeSetProposal,
) -> Result<()> {
    let Some(store) = store else {
        return Ok(());
    };
    let observed_digest = format!("{:x}", Sha256::digest(proposal.artifact.content.as_bytes()));
    if observed_digest != proposal.artifact.content_sha256 {
        anyhow::bail!(
            "task changeset {} artifact digest changed before persistence",
            proposal.change_set.id.as_str()
        );
    }
    let operation_seed = format!(
        "{}:{}:{}:{}",
        request.task.task_id.as_str(),
        request.plan_version,
        request.step.step_id.as_str(),
        proposal.change_set.id.as_str()
    );
    let operation_id = format!(
        "task-changeset-artifact-{}",
        stable_event_uuid("sigil-task-changeset-artifact", &operation_seed)
    );
    let source_path = std::path::PathBuf::from(".sigil-task-artifacts")
        .join(format!("{}.diff", proposal.change_set.id.as_str()));
    let recorder = store.recorder.clone();
    let workspace_id = store.workspace_id.clone();
    let bytes = proposal.artifact.content.as_bytes().to_vec();
    let artifact_ref = tokio::task::spawn_blocking(move || {
        recorder.capture_immutable_content_artifact(
            &workspace_id,
            &operation_id,
            &source_path,
            &bytes,
        )
    })
    .await
    .context("task changeset artifact persistence task failed")??;
    proposal.artifact_ref = artifact_ref;
    Ok(())
}

fn integration_promotion_policy(
    parent_session: &Session,
    task_id: &TaskId,
    workspace_root: &Path,
) -> Result<VerificationPolicy> {
    let projection = parent_session.verification_state_projection();
    let task_scope = EvidenceScope::Task(task_id.as_str().to_owned());
    let workspace_scope = EvidenceScope::Workspace(stable_workspace_id(workspace_root)?);
    Ok(projection
        .latest_policy(&task_scope)
        .or_else(|| projection.latest_policy(&workspace_scope))
        .map(|entry| entry.policy.clone())
        .unwrap_or_else(|| {
            VerificationPolicy::no_checks_required(DEFAULT_TASK_VERIFICATION_SCOPE_HASH)
        }))
}

async fn persist_integration_promotion_artifact(
    parent_session: &Session,
    workspace_root: &Path,
    prepared: &PreparedGitIntegrationPromotion,
) -> Result<String> {
    let recorder = parent_session
        .mutation_event_recorder()
        .ok_or_else(|| anyhow::anyhow!("integration promotion review requires durable storage"))?;
    let content = prepared.aggregate().artifact.content.as_bytes().to_vec();
    let observed_digest = format!("{:x}", Sha256::digest(&content));
    if observed_digest != prepared.aggregate().artifact.content_sha256 {
        anyhow::bail!("integration promotion aggregate digest changed before persistence");
    }
    let operation_id = format!(
        "task-promotion-preview-artifact-{}",
        stable_event_uuid(
            "sigil-task-promotion-preview-artifact",
            &format!(
                "{}:{}",
                prepared.plan_id().as_str(),
                prepared.aggregate().artifact.content_sha256
            )
        )
    );
    let source_path = std::path::PathBuf::from(".sigil-task-artifacts")
        .join(format!("{}.promotion.diff", prepared.plan_id().as_str()));
    let workspace_id = stable_workspace_id(workspace_root)?;
    tokio::task::spawn_blocking(move || {
        recorder.capture_immutable_content_artifact(
            &workspace_id,
            &operation_id,
            &source_path,
            &content,
        )
    })
    .await
    .context("integration promotion artifact persistence task failed")?
}

async fn promotion_preview_from_prepared(
    parent_session: &Session,
    workspace_root: &Path,
    plan: &sigil_kernel::IntegrationPlan,
    prepared: &mut PreparedGitIntegrationPromotion,
) -> Result<TaskPromotionPreview> {
    let artifact_ref =
        persist_integration_promotion_artifact(parent_session, workspace_root, prepared).await?;
    prepared.bind_aggregate_artifact_ref(artifact_ref.clone())?;
    let policy = integration_promotion_policy(parent_session, &plan.task_id, workspace_root)?;
    let policy_digest = policy.stable_hash()?;
    let projection = IntegrationProjection::from_entries(parent_session.entries());
    let state = projection
        .plans
        .get(&plan.plan_id)
        .ok_or_else(|| anyhow::anyhow!("integration plan disappeared before promotion review"))?;
    build_task_promotion_preview(
        state,
        TaskPromotionPreviewInput {
            aggregate_diff_artifact_ref: artifact_ref,
            aggregate_diff_digest: prepared.aggregate_diff_digest(),
            target: prepared.target().clone(),
            verification_invalidation: vec![policy.verification_scope.scope_hash.clone()],
            intent_binding: None,
            policy_digest,
            has_pending_approval: false,
            has_executable_intent_refs: false,
            created_at_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .context("system clock is before unix epoch")?
                .as_millis()
                .try_into()
                .context("promotion preview timestamp exceeds u64")?,
        },
    )
}

async fn prepare_integration_promotion_preview(
    parent_session: &Session,
    workspace_root: &Path,
    plan: &sigil_kernel::IntegrationPlan,
    artifacts: Vec<IntegrationArtifact>,
    frozen_base: Option<FrozenGitWorktreeBase>,
) -> Result<TaskPromotionPreview> {
    let preparation_id = format!(
        "review-{}",
        stable_event_uuid(
            "sigil-task-promotion-preview",
            &format!(
                "{}:{}:{}",
                plan.task_id.as_str(),
                plan.plan_id.as_str(),
                plan.plan_version
            )
        )
    );
    let mut prepared =
        prepare_git_integration_promotion(GitIntegrationPromotionPreparationRequest {
            preparation_id,
            parent_workspace_root: workspace_root.to_path_buf(),
            plan: plan.clone(),
            artifacts,
            frozen_base,
            target: IntegrationPromotionPreparationTarget::WorkspaceApply {
                expected_snapshot_id: plan.base_snapshot_id.clone(),
                expected_revision: 0,
            },
        })
        .await?;
    let preview =
        promotion_preview_from_prepared(parent_session, workspace_root, plan, &mut prepared).await;
    let cleanup = prepared.cleanup().await;
    match (preview, cleanup) {
        (Ok(preview), Ok(())) => Ok(preview),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(cleanup)) => Err(cleanup.context(
            "integration promotion preview candidate cleanup failed after preview preparation",
        )),
        (Err(error), Err(cleanup)) => Err(error.context(format!(
            "integration promotion preview candidate cleanup also failed: {cleanup:#}"
        ))),
    }
}

fn task_worktree_id(request: &TaskChildSessionRunRequest) -> String {
    let seed = format!(
        "{}:{}:{}:{}",
        request.task.task_id.as_str(),
        request.plan_version,
        request.step.step_id.as_str(),
        request.attempt_id.as_str()
    );
    format!(
        "worktree-{}",
        stable_event_uuid("sigil-task-worktree", &seed)
    )
}

fn task_worktree_changeset_id(request: &TaskChildSessionRunRequest) -> Result<ChangeSetId> {
    let seed = format!(
        "{}:{}:{}:{}",
        request.task.task_id.as_str(),
        request.plan_version,
        request.step.step_id.as_str(),
        request.attempt_id.as_str()
    );
    ChangeSetId::new(format!(
        "changeset-{}",
        stable_event_uuid("sigil-task-worktree-changeset", &seed)
    ))
}

pub(super) fn bind_child_integration_facts(
    child_session: &Session,
    proposal: &mut sigil_kernel::TaskChildChangeSetProposal,
) -> Result<()> {
    let completed_calls = child_session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.status == ToolExecutionStatus::Completed =>
            {
                Some((execution.call_id.clone(), execution.tool_name.clone()))
            }
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    let operations = child_session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
                if completed_calls.contains_key(&approval.call_id) =>
            {
                approval
                    .operation
                    .map(|operation| (approval.call_id.clone(), operation))
            }
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    let mut observed_effects = proposal
        .integration_facts
        .observed_effects
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    for (call_id, tool_name) in &completed_calls {
        match operations.get(call_id) {
            Some(
                ToolOperation::ExecuteUnknownCommand
                | ToolOperation::ExecuteMutatingCommand
                | ToolOperation::ExecuteDestructiveCommand
                | ToolOperation::SendTerminalInput,
            ) => {
                observed_effects.insert(IntegrationObservedEffect::UnknownShell);
            }
            None if matches!(
                tool_name.as_str(),
                "bash" | "terminal_start" | "terminal_input"
            ) =>
            {
                observed_effects.insert(IntegrationObservedEffect::UnknownShell);
            }
            Some(ToolOperation::ExecuteWorkspaceCheckCommand) => {
                observed_effects.insert(IntegrationObservedEffect::Build);
            }
            Some(ToolOperation::InvokePlugin) => {
                observed_effects.insert(IntegrationObservedEffect::Unknown);
            }
            _ => {}
        }
    }
    let child_verification_refs = child_session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::VerificationRecorded(recorded)) => {
                Some(recorded.receipt.receipt.receipt_id.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let declared_effect = if observed_effects.iter().any(|effect| {
        matches!(
            effect,
            IntegrationObservedEffect::Package
                | IntegrationObservedEffect::Build
                | IntegrationObservedEffect::Git
                | IntegrationObservedEffect::Formatter
                | IntegrationObservedEffect::Codegen
                | IntegrationObservedEffect::UnknownShell
                | IntegrationObservedEffect::Unknown
        )
    }) {
        IntegrationEffect::Global
    } else if observed_effects.contains(&IntegrationObservedEffect::SharedGeneratedRoot) {
        IntegrationEffect::GeneratedArtifacts
    } else {
        proposal.integration_facts.declared_effect
    };
    let content_class = if proposal.source_isolation == WriteIsolationMode::Worktree {
        IntegrationContentClass::Text
    } else {
        IntegrationContentClass::Unknown
    };
    proposal.integration_facts = IntegrationProposalFacts::from_changeset(
        &proposal.change_set,
        proposal.integration_facts.base_representation.clone(),
        content_class,
        declared_effect,
        observed_effects.into_iter().collect(),
        proposal.artifact_ref.clone(),
        child_verification_refs,
    )?;
    Ok(())
}

async fn cleanup_task_worktree<H>(
    parent_session: &mut Session,
    handler: &mut H,
    materialized: MaterializedGitWorktree,
) -> Result<Option<String>>
where
    H: EventHandler + Send + ?Sized,
{
    let isolated_workspace_id = materialized.isolated_workspace_id().to_owned();
    match materialized.cleanup().await {
        Ok(receipt) => {
            append_control(
                parent_session,
                handler,
                ControlEntry::IsolatedWorkspaceCleanupRecorded(IsolatedWorkspaceCleanupRecorded {
                    isolated_workspace_id,
                    status: receipt.status,
                }),
            )?;
            Ok(None)
        }
        Err(error) => {
            append_control(
                parent_session,
                handler,
                ControlEntry::IsolatedWorkspaceCleanupRecorded(IsolatedWorkspaceCleanupRecorded {
                    isolated_workspace_id,
                    status: IsolatedWorkspaceCleanupStatus::Failed,
                }),
            )?;
            let message = format!("isolated task worktree cleanup incomplete: {error:#}");
            let _ = handler.handle(RunEvent::Notice(message.clone()));
            Ok(Some(message))
        }
    }
}

async fn cleanup_task_worktree_detached(
    materialized: MaterializedGitWorktree,
) -> (ControlEntry, Option<String>) {
    let isolated_workspace_id = materialized.isolated_workspace_id().to_owned();
    match materialized.cleanup().await {
        Ok(receipt) => (
            ControlEntry::IsolatedWorkspaceCleanupRecorded(IsolatedWorkspaceCleanupRecorded {
                isolated_workspace_id,
                status: receipt.status,
            }),
            None,
        ),
        Err(error) => {
            let message = format!("isolated task worktree cleanup incomplete: {error:#}");
            (
                ControlEntry::IsolatedWorkspaceCleanupRecorded(IsolatedWorkspaceCleanupRecorded {
                    isolated_workspace_id,
                    status: IsolatedWorkspaceCleanupStatus::Failed,
                }),
                Some(message),
            )
        }
    }
}

struct PreparedParallelTaskChild {
    request: TaskChildSessionRunRequest,
    child_task_id: TaskId,
    child_session_ref: SessionRef,
    agent: Arc<BoxedAgent>,
    child_thread: AgentTaskChildThread,
    child_session: Session,
    worktree: Option<MaterializedGitWorktree>,
    parent_options: Option<AgentRunOptions>,
    changeset_artifact_store: Option<ChangesetArtifactStore>,
    _thread_release: TaskChildThreadReleaseGuard,
}

struct ParallelTaskChildSuccess {
    materialized: super::AgentResultMaterialization,
    outcome: sigil_kernel::AgentRunOutcome,
    usage: AgentUsageSummary,
    changeset_proposal: Option<sigil_kernel::TaskChildChangeSetProposal>,
}

struct ExecutedParallelTaskChild {
    prepared: PreparedParallelTaskChild,
    controls: Vec<ControlEntry>,
    result: Result<ParallelTaskChildSuccess>,
}

struct ParallelTaskCompletionContext {
    request_index: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ParallelTaskBatchKind {
    SharedReadOnly,
    ChangesetOnly,
    Worktree,
}

impl ParallelTaskBatchKind {
    fn for_step(step: &TaskStepSpec) -> Result<Self> {
        if matches!(
            step.effective_mode(),
            TaskStepMode::Read | TaskStepMode::Review | TaskStepMode::Verify
        ) && step.effective_isolation() == sigil_kernel::TaskIsolationMode::SharedReadOnly
        {
            return Ok(Self::SharedReadOnly);
        }
        if step.role == AgentRole::SubagentWrite && step.effective_mode() == TaskStepMode::Write {
            return match step.effective_isolation() {
                sigil_kernel::TaskIsolationMode::ChangesetOnly => Ok(Self::ChangesetOnly),
                sigil_kernel::TaskIsolationMode::Worktree => Ok(Self::Worktree),
                _ => anyhow::bail!(
                    "parallel task child writer requires changeset-only or worktree isolation"
                ),
            };
        }
        anyhow::bail!(
            "parallel task child batch accepts only shared-read-only, changeset-only, or worktree steps"
        )
    }
}

struct TaskChildThreadReleaseGuard {
    supervisor: AgentSupervisor,
    thread_id: AgentThreadId,
}

impl TaskChildThreadReleaseGuard {
    fn new(supervisor: &AgentSupervisor, thread: &AgentTaskChildThread) -> Self {
        Self {
            supervisor: supervisor.clone(),
            thread_id: thread.thread_id.clone(),
        }
    }
}

impl Drop for TaskChildThreadReleaseGuard {
    fn drop(&mut self) {
        self.supervisor.release_runtime_thread(&self.thread_id);
    }
}

fn participant_control_step(step_id: &str, title: &str, role: AgentRole) -> Result<TaskStepSpec> {
    Ok(TaskStepSpec {
        step_id: TaskStepId::new(step_id)?,
        title: title.to_owned(),
        display_name: None,
        detail: None,
        role,
        depends_on: Vec::new(),
        mode: Some(TaskStepMode::Read),
        isolation: Some(sigil_kernel::TaskIsolationMode::SharedReadOnly),
    })
}

fn append_integration_runtime_event<H>(
    parent_session: &mut Session,
    handler: &mut H,
    parent_workspace_id: &str,
    plan: &sigil_kernel::IntegrationPlan,
    frozen_base: Option<&FrozenGitWorktreeBase>,
    event: IntegrationLaneRuntimeEvent,
) -> Result<Option<String>>
where
    H: EventHandler + Send,
{
    match event {
        IntegrationLaneRuntimeEvent::Prepared {
            entry,
            materialized_snapshot_id,
        } => {
            let base_commit = match &plan.base_representation {
                sigil_kernel::IntegrationBaseRepresentation::CleanCommit { base_commit }
                | sigil_kernel::IntegrationBaseRepresentation::SnapshotWorkspace {
                    base_commit,
                    ..
                } => base_commit.clone(),
                sigil_kernel::IntegrationBaseRepresentation::Unknown => {
                    anyhow::bail!("integration lane prepared without a complete base");
                }
            };
            append_control(
                parent_session,
                handler,
                ControlEntry::IsolatedWorkspaceCreated(IsolatedWorkspaceCreated {
                    isolated_workspace_id: entry.owned_workspace_id.clone(),
                    parent_workspace_id: parent_workspace_id.to_owned(),
                    owner_agent_id: format!(
                        "integration:{}:{}",
                        entry.plan_id.as_str(),
                        entry.lane_id.as_str()
                    ),
                    isolation_mode: WriteIsolationMode::Worktree,
                    base_snapshot_id: plan.base_snapshot_id.clone(),
                    backend: IsolatedWorkspaceBackend::GitWorktree,
                    base_commit: Some(base_commit),
                    overlay_digest: frozen_base.map(|frozen| frozen.overlay_digest().to_owned()),
                    overlay_artifact_ref: frozen_base
                        .map(|frozen| frozen.overlay_artifact_ref().clone()),
                    overlay_content_artifact_refs: frozen_base
                        .map(FrozenGitWorktreeBase::overlay_content_artifact_refs)
                        .unwrap_or_default(),
                    overlay_entry_count: frozen_base
                        .map_or(0, FrozenGitWorktreeBase::overlay_entry_count),
                    materialized_snapshot_id: Some(materialized_snapshot_id),
                }),
            )?;
            append_control(
                parent_session,
                handler,
                ControlEntry::IntegrationLanePrepared(entry),
            )?;
            Ok(None)
        }
        IntegrationLaneRuntimeEvent::MemberApplied(entry) => {
            append_control(
                parent_session,
                handler,
                ControlEntry::IntegrationLaneMemberApplied(entry),
            )?;
            Ok(None)
        }
        IntegrationLaneRuntimeEvent::VerificationLinked(entry) => {
            append_control(
                parent_session,
                handler,
                ControlEntry::IntegrationLaneVerificationLinked(entry),
            )?;
            Ok(None)
        }
        IntegrationLaneRuntimeEvent::Terminal(entry) => {
            append_control(
                parent_session,
                handler,
                ControlEntry::IntegrationLaneTerminal(entry),
            )?;
            Ok(None)
        }
        IntegrationLaneRuntimeEvent::CleanupRecorded {
            entry,
            workspace_status,
        } => {
            let workspace_id = entry.owned_workspace_id.clone();
            append_control(
                parent_session,
                handler,
                ControlEntry::IntegrationLaneCleanupRecorded(entry),
            )?;
            append_control(
                parent_session,
                handler,
                ControlEntry::IsolatedWorkspaceCleanupRecorded(IsolatedWorkspaceCleanupRecorded {
                    isolated_workspace_id: workspace_id.clone(),
                    status: workspace_status,
                }),
            )?;
            Ok(Some(workspace_id))
        }
    }
}

#[async_trait]
impl TaskChildSessionRunner for AgentSupervisorTaskChildRunner {
    fn supports_integration_lanes(&self) -> bool {
        true
    }

    async fn run_integration_lanes<H>(
        &self,
        parent_session: &mut Session,
        request: TaskIntegrationRunRequest,
        handler: &mut H,
    ) -> Result<TaskIntegrationRunOutput>
    where
        H: EventHandler + Send,
    {
        let parent_workspace_id = stable_workspace_id(&request.workspace_root)?;
        let frozen_base = match &request.plan.base_representation {
            sigil_kernel::IntegrationBaseRepresentation::CleanCommit { .. } => None,
            sigil_kernel::IntegrationBaseRepresentation::SnapshotWorkspace { .. } => Some(
                self.freeze_integration_worktree_base(parent_session, &request)
                    .await?,
            ),
            sigil_kernel::IntegrationBaseRepresentation::Unknown => {
                anyhow::bail!("integration plan is missing its physical base representation");
            }
        };
        for lane in &request.plan.lanes {
            let isolated_workspace_id =
                git_integration_workspace_id(&request.plan.plan_id, &lane.lane_id);
            append_control(
                parent_session,
                handler,
                ControlEntry::IsolatedWorkspacePrepared(IsolatedWorkspacePrepared {
                    isolated_workspace_id,
                    parent_workspace_id: parent_workspace_id.clone(),
                    owner_agent_id: format!(
                        "integration:{}:{}",
                        request.plan.plan_id.as_str(),
                        lane.lane_id.as_str()
                    ),
                    isolation_mode: WriteIsolationMode::Worktree,
                    base_snapshot_id: request.plan.base_snapshot_id.clone(),
                    backend: IsolatedWorkspaceBackend::GitWorktree,
                    base_commit: Some(match &request.plan.base_representation {
                        sigil_kernel::IntegrationBaseRepresentation::CleanCommit {
                            base_commit,
                        }
                        | sigil_kernel::IntegrationBaseRepresentation::SnapshotWorkspace {
                            base_commit,
                            ..
                        } => base_commit.clone(),
                        sigil_kernel::IntegrationBaseRepresentation::Unknown => unreachable!(),
                    }),
                    overlay_digest: frozen_base
                        .as_ref()
                        .map(|frozen| frozen.overlay_digest().to_owned()),
                    overlay_artifact_ref: frozen_base
                        .as_ref()
                        .map(|frozen| frozen.overlay_artifact_ref().clone()),
                    overlay_content_artifact_refs: frozen_base
                        .as_ref()
                        .map(FrozenGitWorktreeBase::overlay_content_artifact_refs)
                        .unwrap_or_default(),
                    overlay_entry_count: frozen_base
                        .as_ref()
                        .map_or(0, FrozenGitWorktreeBase::overlay_entry_count),
                }),
            )?;
            append_control(
                parent_session,
                handler,
                ControlEntry::IntegrationLaneChanged(IntegrationLaneChanged {
                    plan_id: request.plan.plan_id.clone(),
                    lane_id: lane.lane_id.clone(),
                    status: IntegrationLaneStatus::Integrating,
                    candidate: None,
                    verification_check_ids: Vec::new(),
                    reason: None,
                }),
            )?;
        }
        let artifacts = request
            .proposals
            .into_iter()
            .map(|proposal| {
                if proposal.proposal.artifact.media_type != "text/x-diff" {
                    anyhow::bail!(
                        "integration proposal {} has unsupported artifact media type {}",
                        proposal.proposal.change_set.id.as_str(),
                        proposal.proposal.artifact.media_type
                    );
                }
                Ok(IntegrationArtifact {
                    change_set: proposal.proposal.change_set,
                    content: proposal.proposal.artifact.content,
                    content_sha256: proposal.proposal.artifact.content_sha256,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let (event_sender, mut event_receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut physical_run = Box::pin(run_git_integration_lanes_with_events(
            GitIntegrationRunRequest {
                parent_workspace_root: request.workspace_root.clone(),
                plan: request.plan.clone(),
                artifacts: artifacts.clone(),
                frozen_base: frozen_base.clone(),
                verification_backend: Some(self.integration_verification_backend.clone()),
            },
            Some(event_sender),
        ));
        let mut event_error: Option<String> = None;
        let mut cleanup_recorded = BTreeSet::new();
        let output = loop {
            tokio::select! {
                request_event = event_receiver.recv() => {
                    let Some(request_event) = request_event else {
                        break physical_run.await;
                    };
                    if let Some(error) = event_error.as_ref() {
                        request_event.acknowledge(Err(error.clone()));
                        continue;
                    }
                    let event = request_event.event().clone();
                    let append_result = append_integration_runtime_event(
                        parent_session,
                        handler,
                        &parent_workspace_id,
                        &request.plan,
                        frozen_base.as_ref(),
                        event,
                    );
                    match append_result {
                        Ok(cleanup_id) => {
                            if let Some(cleanup_id) = cleanup_id {
                                cleanup_recorded.insert(cleanup_id);
                            }
                            request_event.acknowledge(Ok(()));
                        }
                        Err(error) => {
                            let message = format!("{error:#}");
                            request_event.acknowledge(Err(message.clone()));
                            event_error = Some(message);
                        }
                    }
                }
                result = &mut physical_run => {
                    while let Ok(request_event) = event_receiver.try_recv() {
                        if let Some(error) = event_error.as_ref() {
                            request_event.acknowledge(Err(error.clone()));
                            continue;
                        }
                        let event = request_event.event().clone();
                        match append_integration_runtime_event(
                            parent_session,
                            handler,
                            &parent_workspace_id,
                            &request.plan,
                            frozen_base.as_ref(),
                            event,
                        ) {
                            Ok(cleanup_id) => {
                                if let Some(cleanup_id) = cleanup_id {
                                    cleanup_recorded.insert(cleanup_id);
                                }
                                request_event.acknowledge(Ok(()));
                            }
                            Err(error) => {
                                let message = format!("{error:#}");
                                request_event.acknowledge(Err(message.clone()));
                                event_error = Some(message);
                            }
                        }
                    }
                    break result;
                }
            }
        };
        if let Some(error) = event_error {
            anyhow::bail!("failed to persist integration lane lifecycle: {error}");
        }
        let output = match output {
            Ok(output) => output,
            Err(error) => {
                for lane in &request.plan.lanes {
                    let isolated_workspace_id =
                        git_integration_workspace_id(&request.plan.plan_id, &lane.lane_id);
                    if cleanup_recorded.contains(&isolated_workspace_id) {
                        continue;
                    }
                    append_control(
                        parent_session,
                        handler,
                        ControlEntry::IsolatedWorkspaceCleanupRecorded(
                            IsolatedWorkspaceCleanupRecorded {
                                isolated_workspace_id,
                                status: IsolatedWorkspaceCleanupStatus::Failed,
                            },
                        ),
                    )?;
                }
                return Err(error);
            }
        };
        let mut lanes = Vec::with_capacity(output.lanes.len());
        for lane in output.lanes {
            lanes.push(IntegrationLaneChanged {
                plan_id: lane.plan_id,
                lane_id: lane.lane_id,
                status: lane.status,
                candidate: lane.candidate,
                verification_check_ids: lane.verification_check_ids,
                reason: lane.reason,
            });
        }
        let promotion_preview = if lanes
            .iter()
            .all(|lane| lane.status == IntegrationLaneStatus::Ready)
        {
            Some(
                prepare_integration_promotion_preview(
                    parent_session,
                    &request.workspace_root,
                    &request.plan,
                    artifacts,
                    frozen_base,
                )
                .await?,
            )
        } else {
            None
        };
        Ok(TaskIntegrationRunOutput {
            lanes,
            promotion_preview,
        })
    }

    async fn run_planner_session<H, A>(
        &self,
        parent_session: &mut Session,
        request: TaskPlannerSessionRunRequest,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<TaskPlannerSessionRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        let planner = self
            .planner
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("task planner role is not configured"))?;
        let step = participant_control_step("planner", "Plan task", AgentRole::Planner)?;
        let (mut child_session, child_thread, _child_task_id) = self.begin_isolated_participant(
            parent_session,
            handler,
            &request.task,
            request.attempt_id.clone(),
            0,
            request.child_session_ref.clone(),
            request.child_input.clone(),
            request.options.clone(),
            step,
            planner,
        )?;
        let _thread_release = TaskChildThreadReleaseGuard::new(&self.supervisor, &child_thread);
        let planner_run = {
            let mut participant_handler = TaskParticipantEventHandler { inner: handler };
            if self.planner_discovery_max_probes == 0 {
                planner
                    .run_with_approval_input(
                        &mut child_session,
                        request.child_input.clone(),
                        request.options.clone(),
                        &mut participant_handler,
                        approval_handler,
                    )
                    .await
            } else {
                let tools = planner_tools_with_discovery(
                    planner.tool_registry(),
                    self.planner_discovery_max_probes,
                );
                let mut discovery_delegate = TaskDiscoveryDelegate::new(
                    self.supervisor.clone(),
                    parent_session,
                    request.task.clone(),
                    request.attempt_id.clone(),
                    child_thread.thread_id.clone(),
                    Arc::clone(&self.subagent_read),
                    request.discovery_options.clone(),
                    self.planner_discovery_max_probes,
                );
                planner
                    .run_with_approval_input_tool_registry_and_agent_delegate(
                        &mut child_session,
                        request.child_input.clone(),
                        request.options.clone(),
                        tools,
                        &mut participant_handler,
                        approval_handler,
                        &mut discovery_delegate,
                    )
                    .await
            }
        };
        let output = match planner_run {
            Ok(output) => output,
            Err(error) => {
                let error = self.retryable_zero_effect_error(
                    &request.attempt_id,
                    &request.child_input,
                    planner,
                    &child_session,
                    error,
                );
                self.supervisor.record_task_child_failure(
                    parent_session,
                    handler,
                    &child_thread,
                    format!("{error:#}"),
                )?;
                return Err(error);
            }
        };
        let postprocessed = (|| -> Result<TaskPlannerSessionRunOutput> {
            let accepted_plan = child_session
                .task_state_projection()
                .tasks
                .get(&request.task.task_id)
                .and_then(|task| task.latest_plan_version)
                .and_then(|version| {
                    child_session
                        .entries()
                        .iter()
                        .rev()
                        .find_map(|entry| match entry {
                            sigil_kernel::SessionLogEntry::Control(ControlEntry::TaskPlan(
                                plan,
                            )) if plan.task_id == request.task.task_id
                                && plan.plan_version == version
                                && plan.status == sigil_kernel::TaskPlanStatus::Accepted =>
                            {
                                Some(plan.clone())
                            }
                            _ => None,
                        })
                })
                .ok_or_else(|| {
                    anyhow::anyhow!("isolated planner did not produce an accepted plan")
                })?;
            let guidance_applied =
                child_session
                    .entries()
                    .iter()
                    .rev()
                    .find_map(|entry| match entry {
                        sigil_kernel::SessionLogEntry::Control(
                            ControlEntry::TaskGuidanceApplied(applied),
                        ) if applied.task_id == request.task.task_id => Some(applied.clone()),
                        _ => None,
                    });
            let materialized = super::AgentResultMaterialization::inline(
                format!("accepted task plan v{}", accepted_plan.plan_version),
                None,
            );
            self.supervisor.record_task_child_result(
                parent_session,
                handler,
                &child_thread,
                request.child_session_ref.clone(),
                TaskChildSessionStatus::Completed,
                &materialized,
                &output.outcome,
                Some(usage_summary_from_stats(child_session.stats())),
            )?;
            Ok(TaskPlannerSessionRunOutput {
                attempt_id: request.attempt_id.clone(),
                accepted_plan,
                guidance_applied,
                child_session_ref: request.child_session_ref.clone(),
            })
        })();
        match postprocessed {
            Ok(output) => Ok(output),
            Err(error) => {
                self.supervisor.record_task_child_failure(
                    parent_session,
                    handler,
                    &child_thread,
                    format!("{error:#}"),
                )?;
                Err(error)
            }
        }
    }

    async fn run_child_session<H, A>(
        &self,
        parent_session: &mut Session,
        request: TaskChildSessionRunRequest,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<TaskChildSessionRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        let mut request = request;
        let parent_options = request.options.clone();
        let changeset_artifact_store =
            changeset_artifact_store(parent_session, &parent_options.workspace_root, &request)?;
        let worktree =
            if request.step.effective_isolation() == sigil_kernel::TaskIsolationMode::Worktree {
                if request.step.role != AgentRole::SubagentWrite
                    || request.step.effective_mode() != TaskStepMode::Write
                {
                    anyhow::bail!("worktree task child requires a subagent_write write step");
                }
                let frozen = self
                    .freeze_task_worktree_base(parent_session, &request)
                    .await?;
                let materialized = self
                    .prepare_task_worktree(parent_session, handler, &request, &frozen)
                    .await?;
                request.options.workspace_root = materialized.workspace_root().to_path_buf();
                request.options.permission_context.workspace_root =
                    materialized.workspace_root().to_path_buf();
                Some(materialized)
            } else {
                None
            };
        let run_result: Result<TaskChildSessionRunOutput> = async {
            let child_task_id =
                task_participant_child_task_id(&request.task.task_id, &request.attempt_id)?;
            let child_session_ref = request.child_session_ref.clone();
            let agent = match request.step.role {
                AgentRole::Planner => self
                    .planner
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("task planner role is not configured"))?,
                AgentRole::Executor => self
                    .executor
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("task executor role is not configured"))?,
                AgentRole::SubagentRead => &self.subagent_read,
                AgentRole::SubagentWrite => &self.subagent_write,
            };
            let child_thread = self.supervisor.begin_task_child_thread(
                parent_session,
                handler,
                AgentTaskChildStart {
                    task_id: request.task.task_id.clone(),
                    parent_thread_id: main_thread_id()?,
                    parent_depth: 0,
                    batch_id: None,
                    batch_member_key: None,
                    parent_session_ref: request.task.parent_session_ref.clone(),
                    plan_version: request.plan_version,
                    step: request.step.clone(),
                    child_task_id: child_task_id.clone(),
                    child_session_ref: child_session_ref.clone(),
                    child_input: request.child_input.clone(),
                    objective: request.task.objective.clone(),
                    workspace_root: request.options.workspace_root.clone(),
                    provider_capabilities: child_provider_capabilities(agent),
                    role: request.step.role,
                    invocation_mode: AgentInvocationMode::Foreground,
                    invocation_source: AgentInvocationSource::Task,
                    isolated_workspace_id: worktree
                        .as_ref()
                        .map(|workspace| workspace.isolated_workspace_id().to_owned()),
                },
            )?;
            let _thread_release = TaskChildThreadReleaseGuard::new(&self.supervisor, &child_thread);
            append_task_child_session(
                parent_session,
                handler,
                &request,
                &child_task_id,
                &child_session_ref,
                TaskChildSessionStatus::Started,
                None,
            )?;
            let mut child_session = match build_child_session(parent_session, &child_session_ref) {
                Ok(session) => session,
                Err(error) => {
                    append_task_child_session(
                        parent_session,
                        handler,
                        &request,
                        &child_task_id,
                        &child_session_ref,
                        TaskChildSessionStatus::Failed,
                        None,
                    )?;
                    self.supervisor.record_task_child_failure(
                        parent_session,
                        handler,
                        &child_thread,
                        format!("{error:#}"),
                    )?;
                    return Err(error);
                }
            };
            let mut route_handler = SupervisorTaskApprovalRouteHandler {
                inner: approval_handler,
                parent_session,
                task_request: &request,
                child_session_ref: &child_session_ref,
                source_thread_id: &child_thread.thread_id,
            };
            let child_input = request.child_input.clone();
            let options = request.options.clone();
            let child_run = {
                let mut participant_handler = TaskParticipantEventHandler { inner: handler };
                run_task_child_agent_for_step(
                    agent,
                    &mut child_session,
                    child_input,
                    options,
                    &request.step,
                    &mut participant_handler,
                    &mut route_handler,
                )
                .await
            };
            let output = match child_run {
                Ok(output) => output,
                Err(error) => {
                    let error = self.retryable_child_error(&request, agent, &child_session, error);
                    append_task_child_session(
                        route_handler.parent_session,
                        handler,
                        &request,
                        &child_task_id,
                        &child_session_ref,
                        TaskChildSessionStatus::Failed,
                        None,
                    )?;
                    self.supervisor.record_task_child_failure(
                        route_handler.parent_session,
                        handler,
                        &child_thread,
                        format!("{error:#}"),
                    )?;
                    return Err(error);
                }
            };
            let postprocessed = async {
                let mut changeset_proposal = match request.step.effective_isolation() {
                    sigil_kernel::TaskIsolationMode::ChangesetOnly => Some(
                        decode_changeset_only_child_output(&output.result.final_text)?,
                    ),
                    sigil_kernel::TaskIsolationMode::Worktree => {
                        let workspace = worktree.as_ref().ok_or_else(|| {
                            anyhow::anyhow!("worktree task child lost its materialization receipt")
                        })?;
                        workspace
                            .extract_changeset(
                                task_worktree_changeset_id(&request)?,
                                request.step.title.clone(),
                                format!(
                                    "Isolated worktree changes for task step {}",
                                    request.step.step_id.as_str()
                                ),
                            )
                            .await?
                    }
                    _ => None,
                };
                if let Some(proposal) = &mut changeset_proposal {
                    persist_changeset_artifact(
                        changeset_artifact_store.as_ref(),
                        &request,
                        proposal,
                    )
                    .await?;
                    bind_child_integration_facts(&child_session, proposal)?;
                }
                let isolated_parent_snapshot_id =
                    if let Some(base_snapshot_id) = request.isolated_base_snapshot_id.as_deref() {
                        Some(
                            sigil_kernel::validate_isolated_parent_snapshot_unchanged_for_task(
                                route_handler.parent_session,
                                &request.task,
                                request.plan_version,
                                &request.step,
                                if worktree.is_some() {
                                    &parent_options
                                } else {
                                    &request.options
                                },
                                base_snapshot_id,
                            )?,
                        )
                    } else {
                        None
                    };
                let materialized = materialize_child_agent_final_answer(
                    &mut child_session,
                    &child_session_ref,
                    &child_thread.thread_id,
                    &output.result,
                )
                .await?;
                let outcome = output.outcome;
                let usage = usage_summary_from_stats(child_session.stats());
                let budget_warning = self
                    .supervisor
                    .validate_usage_budget(&request.task.task_id, &usage)
                    .err()
                    .map(|error| format!("{error:#}"));
                let status = task_child_status_from_outcome(&materialized.final_text, &outcome);
                append_task_child_session(
                    route_handler.parent_session,
                    handler,
                    &request,
                    &child_task_id,
                    &child_session_ref,
                    status,
                    Some(hash_text(&materialized.final_text)),
                )?;
                self.supervisor.record_task_child_result(
                    route_handler.parent_session,
                    handler,
                    &child_thread,
                    child_session_ref.clone(),
                    status,
                    &materialized,
                    &outcome,
                    Some(usage),
                )?;
                if let Some(warning) = budget_warning {
                    let _ = handler.handle(RunEvent::Notice(format!(
                        "agent budget warning after child completion: {warning}"
                    )));
                }
                let mut parent_outcome = outcome;
                if worktree.is_some() {
                    parent_outcome.changed_files.clear();
                }
                Ok(TaskChildSessionRunOutput {
                    attempt_id: request.attempt_id.clone(),
                    final_text: materialized.final_text,
                    outcome: parent_outcome,
                    child_session_ref: child_session_ref.clone(),
                    final_answer_ref: materialized.final_answer_ref,
                    artifact_refs: materialized.extra_artifacts,
                    changeset_proposal,
                    isolated_parent_snapshot_id,
                })
            }
            .await;
            match postprocessed {
                Ok(output) => Ok(output),
                Err(error) => {
                    append_task_child_session(
                        route_handler.parent_session,
                        handler,
                        &request,
                        &child_task_id,
                        &child_session_ref,
                        TaskChildSessionStatus::Failed,
                        None,
                    )?;
                    self.supervisor.record_task_child_failure(
                        route_handler.parent_session,
                        handler,
                        &child_thread,
                        format!("{error:#}"),
                    )?;
                    Err(error)
                }
            }
        }
        .await;
        let cleanup_error = if let Some(worktree) = worktree {
            cleanup_task_worktree(parent_session, handler, worktree).await?
        } else {
            None
        };
        match (run_result, cleanup_error) {
            (Ok(output), _) => Ok(output),
            (Err(error), Some(cleanup_error)) => Err(error.context(cleanup_error)),
            (Err(error), None) => Err(error),
        }
    }

    fn prepare_child_session_batch<'a, H, A>(
        &'a self,
        parent_session: &mut Session,
        requests: Vec<TaskChildSessionRunRequest>,
        handler: &'a mut H,
        approval_handler: &'a mut A,
    ) -> Result<TaskChildSessionBatchPreparation<'a>>
    where
        H: EventHandler + Send + 'a,
        A: ApprovalHandler + Send + 'a,
    {
        if requests.is_empty() {
            return Ok(detached_task_batch_results(Vec::new()));
        }
        let preflight = match self.preflight_parallel_task_batch(parent_session, &requests) {
            Ok(preflight) => preflight,
            Err(error) => {
                return Ok(detached_task_batch_results(rejected_parallel_task_batch(
                    &requests, error,
                )));
            }
        };
        if preflight.kind == ParallelTaskBatchKind::Worktree {
            return Ok(TaskChildSessionBatchPreparation::Fallback(requests));
        }
        let starts = preflight
            .members
            .iter()
            .map(|member| member.start.clone())
            .collect::<Vec<_>>();
        let reservation = match self.supervisor.reserve_task_child_batch(&starts) {
            Ok(reservation) => reservation,
            Err(error) => {
                return Ok(detached_task_batch_results(rejected_parallel_task_batch(
                    &requests, error,
                )));
            }
        };
        let mut prepared = Vec::with_capacity(preflight.members.len());
        for member in preflight.members {
            match self.start_parallel_task_child(parent_session, member, handler) {
                Ok(member) => prepared.push(member),
                Err(error) => {
                    let reason =
                        "parallel task child batch start rolled back before provider dispatch";
                    for started in &prepared {
                        let _ = append_task_child_session(
                            parent_session,
                            handler,
                            &started.request,
                            &started.child_task_id,
                            &started.child_session_ref,
                            TaskChildSessionStatus::Failed,
                            None,
                        );
                        let _ = self.supervisor.record_task_child_failure(
                            parent_session,
                            handler,
                            &started.child_thread,
                            reason.to_owned(),
                        );
                    }
                    return Ok(detached_task_batch_results(rejected_parallel_task_batch(
                        &requests, error,
                    )));
                }
            }
        }
        reservation.commit();
        self.detach_prepared_task_batch(
            preflight.task_id,
            preflight.plan_version,
            prepared,
            handler,
            approval_handler,
        )
    }

    async fn run_child_session_batch<H, A>(
        &self,
        parent_session: &mut Session,
        requests: Vec<TaskChildSessionRunRequest>,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<Vec<Result<TaskChildSessionRunOutput>>>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        let preparation =
            self.prepare_child_session_batch(parent_session, requests, handler, approval_handler)?;
        let settled = settle_runtime_task_child_batch(preparation).await?;
        match settled {
            SettledRuntimeTaskChildBatch::Detached(commit) => {
                commit.commit(parent_session, handler)
            }
            SettledRuntimeTaskChildBatch::Fallback(requests) => {
                if requests.iter().all(|request| {
                    request.step.effective_isolation() == sigil_kernel::TaskIsolationMode::Worktree
                }) {
                    return self
                        .run_parallel_worktree_batch(
                            parent_session,
                            requests,
                            handler,
                            approval_handler,
                        )
                        .await;
                }
                let mut outputs = Vec::with_capacity(requests.len());
                for request in requests {
                    outputs.push(
                        self.run_child_session(parent_session, request, handler, approval_handler)
                            .await,
                    );
                }
                Ok(outputs)
            }
        }
    }

    async fn run_synthesis_session<H, A>(
        &self,
        parent_session: &mut Session,
        request: TaskSynthesisSessionRunRequest,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<TaskSynthesisSessionRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        let synthesis = self
            .synthesis
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("task synthesis role is not configured"))?;
        let step = participant_control_step("synthesis", "Synthesize task", AgentRole::Planner)?;
        let (mut child_session, child_thread, _child_task_id) = self.begin_isolated_participant(
            parent_session,
            handler,
            &request.task,
            request.attempt_id.clone(),
            request.plan_version,
            request.child_session_ref.clone(),
            request.child_input.clone(),
            request.options.clone(),
            step,
            synthesis,
        )?;
        let _thread_release = TaskChildThreadReleaseGuard::new(&self.supervisor, &child_thread);
        let synthesis_run = {
            let mut participant_handler = TaskParticipantEventHandler { inner: handler };
            synthesis
                .run_with_approval_input(
                    &mut child_session,
                    request.child_input.clone(),
                    request.options.clone(),
                    &mut participant_handler,
                    approval_handler,
                )
                .await
        };
        let output = match synthesis_run {
            Ok(output) => output,
            Err(error) => {
                let error = self.retryable_zero_effect_error(
                    &request.attempt_id,
                    &request.child_input,
                    synthesis,
                    &child_session,
                    error,
                );
                self.supervisor.record_task_child_failure(
                    parent_session,
                    handler,
                    &child_thread,
                    format!("{error:#}"),
                )?;
                return Err(error);
            }
        };
        let postprocessed = (|| -> Result<TaskSynthesisSessionRunOutput> {
            let final_text = sigil_kernel::safe_persistence_text(&output.result.final_text);
            let final_answer_ref = output
                .result
                .final_message_id
                .as_ref()
                .map(|message_id| sigil_kernel::AgentFinalAnswerRef {
                    session_ref: request.child_session_ref.clone(),
                    message_id: message_id.clone(),
                    content_hash: hash_text(&final_text),
                    char_count: final_text.chars().count(),
                })
                .ok_or_else(|| anyhow::anyhow!("synthesis child did not persist a final answer"))?;
            let materialized = super::AgentResultMaterialization::inline(
                final_text.clone(),
                Some(final_answer_ref.clone()),
            );
            let outcome = output.outcome.clone();
            self.supervisor.record_task_child_result(
                parent_session,
                handler,
                &child_thread,
                request.child_session_ref.clone(),
                task_child_status_from_outcome(&final_text, &outcome),
                &materialized,
                &outcome,
                Some(usage_summary_from_stats(child_session.stats())),
            )?;
            Ok(TaskSynthesisSessionRunOutput {
                attempt_id: request.attempt_id.clone(),
                final_text,
                outcome,
                child_session_ref: request.child_session_ref.clone(),
                final_answer_ref,
                artifact_refs: Vec::new(),
            })
        })();
        match postprocessed {
            Ok(output) => Ok(output),
            Err(error) => {
                self.supervisor.record_task_child_failure(
                    parent_session,
                    handler,
                    &child_thread,
                    format!("{error:#}"),
                )?;
                Err(error)
            }
        }
    }
}

fn detached_task_batch_results<'a>(
    results: Vec<Result<TaskChildSessionRunOutput>>,
) -> TaskChildSessionBatchPreparation<'a> {
    let request_count = results.len();
    TaskChildSessionBatchPreparation::Detached(Box::pin(async move {
        Ok(TaskChildSessionBatchCommitEnvelope::new(
            request_count,
            move |_parent_session, _handler| Ok(results),
        ))
    }))
}

enum SettledRuntimeTaskChildBatch {
    Fallback(Vec<TaskChildSessionRunRequest>),
    Detached(TaskChildSessionBatchCommitEnvelope),
}

async fn settle_runtime_task_child_batch(
    preparation: TaskChildSessionBatchPreparation<'_>,
) -> Result<SettledRuntimeTaskChildBatch> {
    match preparation {
        TaskChildSessionBatchPreparation::Fallback(requests) => {
            Ok(SettledRuntimeTaskChildBatch::Fallback(requests))
        }
        TaskChildSessionBatchPreparation::Detached(batch_future) => batch_future
            .await
            .map(SettledRuntimeTaskChildBatch::Detached),
    }
}

fn rejected_parallel_task_batch(
    requests: &[TaskChildSessionRunRequest],
    error: anyhow::Error,
) -> Vec<Result<TaskChildSessionRunOutput>> {
    if let Some(retry) = error.downcast_ref::<TaskParticipantRetryError>() {
        return requests
            .iter()
            .map(|request| {
                let input_hash = task_participant_input_hash(&request.child_input)?;
                Err(anyhow::Error::new(TaskParticipantRetryError::new(
                    retry.retry_after_ms(),
                    retry.route_fingerprint(),
                    input_hash,
                    TaskParticipantRetryProof::AdmissionRejectedBeforeDispatch {
                        zero_output: true,
                        zero_tool: true,
                        zero_effect: true,
                    },
                    anyhow::Error::new(ProviderRouteCooldownError::new(
                        retry.retry_after_ms(),
                        retry.route_fingerprint(),
                    ))
                    .context("parallel task child batch rejected before provider dispatch"),
                )?))
            })
            .collect();
    }
    if let Some(cooldown) = error.downcast_ref::<ProviderRouteCooldownError>().cloned() {
        return (0..requests.len())
            .map(|_| {
                Err(anyhow::Error::new(cooldown.clone())
                    .context("parallel task child batch rejected before provider dispatch"))
            })
            .collect();
    }
    let reason = format!("parallel task child batch rejected before provider dispatch: {error:#}");
    (0..requests.len())
        .map(|_| Err(anyhow::anyhow!(reason.clone())))
        .collect()
}

fn retry_safe_step(step: &TaskStepSpec) -> bool {
    matches!(
        step.effective_mode(),
        TaskStepMode::Read | TaskStepMode::Review | TaskStepMode::Verify
    ) && step.effective_isolation() == sigil_kernel::TaskIsolationMode::SharedReadOnly
}

struct TaskParticipantEventHandler<'a, H> {
    inner: &'a mut H,
}

impl<H> EventHandler for TaskParticipantEventHandler<'_, H>
where
    H: EventHandler,
{
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        match event {
            RunEvent::AssistantMessage(_)
            | RunEvent::TextDelta(_)
            | RunEvent::ReasoningDelta(_)
            | RunEvent::ContinuationState(_)
            | RunEvent::Control(_) => Ok(()),
            event => self.inner.handle(event),
        }
    }
}

struct SharedTaskEventHandler<'a, H> {
    inner: Arc<Mutex<&'a mut H>>,
}

impl<H> Clone for SharedTaskEventHandler<'_, H> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<H> EventHandler for SharedTaskEventHandler<'_, H>
where
    H: EventHandler + Send,
{
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        self.inner
            .lock()
            .map_err(|_| anyhow::anyhow!("task event handler lock poisoned"))?
            .handle(event)
    }
}

struct SharedTaskApprovalHandler<'a, A> {
    inner: Arc<Mutex<&'a mut A>>,
}

impl<A> Clone for SharedTaskApprovalHandler<'_, A> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<A> ApprovalHandler for SharedTaskApprovalHandler<'_, A>
where
    A: ApprovalHandler + Send,
{
    fn approve_tool_call(&mut self, call: &ToolCall, spec: &ToolSpec) -> Result<ToolApproval> {
        self.inner
            .lock()
            .map_err(|_| anyhow::anyhow!("task approval handler lock poisoned"))?
            .approve_tool_call(call, spec)
    }

    fn approval_is_explicit_user_action(&self) -> bool {
        self.inner
            .lock()
            .map(|handler| handler.approval_is_explicit_user_action())
            .unwrap_or(false)
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_task_child_agent_for_step<H, A>(
    agent: &BoxedAgent,
    child_session: &mut Session,
    child_input: AgentRunInput,
    options: AgentRunOptions,
    step: &TaskStepSpec,
    handler: &mut H,
    approval_handler: &mut A,
) -> Result<sigil_kernel::AgentRunOutput>
where
    H: EventHandler + Send,
    A: ApprovalHandler + Send,
{
    if step.effective_isolation() == sigil_kernel::TaskIsolationMode::ChangesetOnly {
        let scoped_tools = changeset_only_child_tool_registry(agent.tool_registry());
        agent
            .run_with_approval_input_and_tool_registry(
                child_session,
                child_input,
                options,
                scoped_tools,
                handler,
                approval_handler,
            )
            .await
    } else {
        agent
            .run_with_approval_input(
                child_session,
                child_input,
                options,
                handler,
                approval_handler,
            )
            .await
    }
}

struct BufferedSupervisorTaskApprovalRouteHandler<'a, A> {
    inner: &'a mut A,
    task_request: &'a TaskChildSessionRunRequest,
    child_session_ref: &'a SessionRef,
    source_thread_id: &'a AgentThreadId,
    controls: Vec<ControlEntry>,
}

impl<A> ApprovalHandler for BufferedSupervisorTaskApprovalRouteHandler<'_, A>
where
    A: ApprovalHandler,
{
    fn approval_is_explicit_user_action(&self) -> bool {
        self.inner.approval_is_explicit_user_action()
    }

    fn approve_tool_call(&mut self, call: &ToolCall, spec: &ToolSpec) -> Result<ToolApproval> {
        let task_route_id = task_route_id_for_call(
            &self.task_request.task.task_id,
            &self.task_request.step.step_id,
            &call.id,
        )?;
        let agent_route_id = agent_route_id_for_call(self.source_thread_id, &call.id)?;
        self.controls.extend([
            task_approval_route_control(
                self.task_request,
                self.child_session_ref,
                task_route_id.clone(),
                call,
                TaskRouteStatus::Requested,
            ),
            ControlEntry::AgentApprovalRoute(AgentApprovalRouteEntry {
                route_id: agent_route_id.clone(),
                source_thread_id: self.source_thread_id.clone(),
                target_thread_id: None,
                call_id: call.id.clone(),
                tool_name: call.name.clone(),
                status: AgentRouteStatus::Requested,
            }),
        ]);
        let approval = self.inner.approve_tool_call(call, spec)?;
        let (task_status, agent_status) = match approval {
            ToolApproval::Approve
            | ToolApproval::ApproveForSession
            | ToolApproval::ApproveWithArgs { .. } => {
                (TaskRouteStatus::Resolved, AgentRouteStatus::Resolved)
            }
            ToolApproval::Deny { .. } => (TaskRouteStatus::Rejected, AgentRouteStatus::Rejected),
        };
        self.controls.extend([
            task_approval_route_control(
                self.task_request,
                self.child_session_ref,
                task_route_id,
                call,
                task_status,
            ),
            ControlEntry::AgentApprovalRoute(AgentApprovalRouteEntry {
                route_id: agent_route_id,
                source_thread_id: self.source_thread_id.clone(),
                target_thread_id: None,
                call_id: call.id.clone(),
                tool_name: call.name.clone(),
                status: agent_status,
            }),
        ]);
        Ok(approval)
    }
}

struct SupervisorTaskApprovalRouteHandler<'a, A> {
    inner: &'a mut A,
    parent_session: &'a mut Session,
    task_request: &'a TaskChildSessionRunRequest,
    child_session_ref: &'a SessionRef,
    source_thread_id: &'a AgentThreadId,
}

impl<A> ApprovalHandler for SupervisorTaskApprovalRouteHandler<'_, A>
where
    A: ApprovalHandler,
{
    fn approval_is_explicit_user_action(&self) -> bool {
        self.inner.approval_is_explicit_user_action()
    }

    fn approve_tool_call(&mut self, call: &ToolCall, spec: &ToolSpec) -> Result<ToolApproval> {
        let task_route_id = task_route_id_for_call(
            &self.task_request.task.task_id,
            &self.task_request.step.step_id,
            &call.id,
        )?;
        let agent_route_id = agent_route_id_for_call(self.source_thread_id, &call.id)?;
        append_task_approval_route(
            self.parent_session,
            self.task_request,
            self.child_session_ref,
            &task_route_id,
            call,
            TaskRouteStatus::Requested,
        )?;
        self.parent_session
            .append_control(ControlEntry::AgentApprovalRoute(AgentApprovalRouteEntry {
                route_id: agent_route_id.clone(),
                source_thread_id: self.source_thread_id.clone(),
                target_thread_id: None,
                call_id: call.id.clone(),
                tool_name: call.name.clone(),
                status: AgentRouteStatus::Requested,
            }))?;
        let approval = self.inner.approve_tool_call(call, spec)?;
        let (task_status, agent_status) = match approval {
            ToolApproval::Approve
            | ToolApproval::ApproveForSession
            | ToolApproval::ApproveWithArgs { .. } => {
                (TaskRouteStatus::Resolved, AgentRouteStatus::Resolved)
            }
            ToolApproval::Deny { .. } => (TaskRouteStatus::Rejected, AgentRouteStatus::Rejected),
        };
        append_task_approval_route(
            self.parent_session,
            self.task_request,
            self.child_session_ref,
            &task_route_id,
            call,
            task_status,
        )?;
        self.parent_session
            .append_control(ControlEntry::AgentApprovalRoute(AgentApprovalRouteEntry {
                route_id: agent_route_id,
                source_thread_id: self.source_thread_id.clone(),
                target_thread_id: None,
                call_id: call.id.clone(),
                tool_name: call.name.clone(),
                status: agent_status,
            }))?;
        Ok(approval)
    }
}

fn append_task_child_session<H>(
    session: &mut Session,
    handler: &mut H,
    request: &TaskChildSessionRunRequest,
    child_task_id: &TaskId,
    child_session_ref: &SessionRef,
    status: TaskChildSessionStatus,
    summary_hash: Option<String>,
) -> Result<()>
where
    H: EventHandler + Send + ?Sized,
{
    append_control(
        session,
        handler,
        ControlEntry::TaskChildSession(TaskChildSessionEntry {
            task_id: request.task.task_id.clone(),
            plan_version: request.plan_version,
            step_id: request.step.step_id.clone(),
            child_task_id: child_task_id.clone(),
            child_session_ref: child_session_ref.clone(),
            role: request.step.role,
            status,
            summary_hash,
        }),
    )
}

fn append_task_approval_route(
    session: &mut Session,
    request: &TaskChildSessionRunRequest,
    child_session_ref: &SessionRef,
    route_id: &TaskRouteId,
    call: &ToolCall,
    status: TaskRouteStatus,
) -> Result<()> {
    session.append_control(task_approval_route_control(
        request,
        child_session_ref,
        route_id.clone(),
        call,
        status,
    ))
}

fn task_approval_route_control(
    request: &TaskChildSessionRunRequest,
    child_session_ref: &SessionRef,
    route_id: TaskRouteId,
    call: &ToolCall,
    status: TaskRouteStatus,
) -> ControlEntry {
    ControlEntry::TaskSubagentApprovalRoute(TaskSubagentApprovalRouteEntry {
        route_id,
        task_id: request.task.task_id.clone(),
        plan_version: request.plan_version,
        step_id: request.step.step_id.clone(),
        role: request.step.role,
        child_session_ref: child_session_ref.clone(),
        call_id: call.id.clone(),
        tool_name: call.name.clone(),
        status,
    })
}

pub(super) fn build_child_session(
    parent_session: &Session,
    child_session_ref: &SessionRef,
) -> Result<Session> {
    if let Some(parent_path) = parent_session.store_path() {
        let parent_dir = parent_path.parent().unwrap_or_else(|| Path::new("."));
        let store = JsonlSessionStore::new(child_session_ref.resolve(parent_dir))?;
        let mut session = Session::load_from_store(
            parent_session.provider_name(),
            parent_session.model_name(),
            store,
        )?;
        crate::attach_session_url_capability_store(&mut session)?;
        return Ok(session);
    }
    let mut session = Session::new(parent_session.provider_name(), parent_session.model_name());
    crate::attach_session_url_capability_store(&mut session)?;
    Ok(session)
}

pub(crate) fn task_child_status_from_outcome(
    final_text: &str,
    outcome: &sigil_kernel::AgentRunOutcome,
) -> TaskChildSessionStatus {
    if outcome.terminal_reason == sigil_kernel::AgentRunTerminalReason::MaxTurns
        || !outcome.interrupted_tool_calls.is_empty()
    {
        TaskChildSessionStatus::Interrupted
    } else if outcome.approval_denials > 0
        || outcome.tool_errors.iter().any(|error| {
            matches!(
                error.kind,
                ToolErrorKind::ApprovalRequired
                    | ToolErrorKind::ApprovalDenied
                    | ToolErrorKind::PermissionDenied
                    | ToolErrorKind::PathOutsideWorkspace
                    | ToolErrorKind::ExternalDirectoryRequired
            )
        })
        || (!outcome.tool_errors.is_empty() && final_text.trim().is_empty())
    {
        TaskChildSessionStatus::Failed
    } else {
        TaskChildSessionStatus::Completed
    }
}

fn child_provider_capabilities(agent: &BoxedAgent) -> ProviderCapabilities {
    agent.provider_capabilities()
}

pub(super) fn usage_summary_from_stats(stats: &SessionStats) -> AgentUsageSummary {
    let input_tokens = stats.prompt_tokens;
    let output_tokens = stats.completion_tokens;
    AgentUsageSummary {
        input_tokens,
        output_tokens,
        total_tokens: input_tokens + output_tokens,
        cached_tokens: Some(stats.cache_hit_tokens),
    }
}

fn main_thread_id() -> Result<AgentThreadId> {
    AgentThreadId::new("main")
}
