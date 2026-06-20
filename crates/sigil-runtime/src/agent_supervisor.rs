use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    Agent, AgentApprovalRouteEntry, AgentArtifactRef, AgentInvocationMode, AgentInvocationSource,
    AgentMergeSafePointEntry, AgentProfileCapturedEntry, AgentProfileId, AgentRole, AgentRouteId,
    AgentRouteStatus, AgentRunAttemptId, AgentRunAttemptStartedEntry, AgentRunContextSnapshot,
    AgentRunInput, AgentRunInterruptedEntry, AgentThreadId, AgentThreadResult,
    AgentThreadResultRecordedEntry, AgentThreadStartedEntry, AgentThreadStatus,
    AgentThreadStatusChangedEntry, AgentThreadTerminalStatus, AgentTrustState, AgentUsageSummary,
    ApprovalHandler, ControlEntry, EventHandler, JsonlSessionStore, Provider, ProviderCapabilities,
    RunEvent, Session, SessionRef, SessionStats, TaskChildSessionEntry, TaskChildSessionRunOutput,
    TaskChildSessionRunRequest, TaskChildSessionRunner, TaskChildSessionStatus, TaskId,
    TaskRouteId, TaskRouteStatus, TaskStepSpec, TaskSubagentApprovalRouteEntry, ToolApproval,
    ToolCall, ToolErrorKind, ToolRegistryScope, ToolSpec, WorkspaceRootSnapshot, child_session_ref,
};

use crate::{
    AgentProfileIndexContext, AgentProfileRegistry, EXPLORE_PROFILE_ID, WORKER_PROFILE_ID,
};

type BoxedAgent = Agent<Box<dyn Provider>>;

const AGENT_RESULT_SUMMARY_LIMIT: usize = 4_000;

/// Runtime-enforced limits for agent/thread fan-out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentBudgetPolicy {
    pub max_threads: usize,
    pub max_depth: usize,
    pub max_parallel_readonly: usize,
    pub max_parallel_write: usize,
    pub max_background_threads: usize,
    pub max_spawn_fanout_per_turn: usize,
    pub max_agent_tokens_per_task: u64,
}

impl AgentBudgetPolicy {
    #[must_use]
    pub fn from_root_config(root_config: &sigil_kernel::RootConfig) -> Self {
        let max_threads = root_config.task.max_child_sessions.max(1);
        Self {
            max_threads,
            max_depth: 1,
            max_parallel_readonly: if root_config.task.allow_parallel_readonly_subagents {
                max_threads
            } else {
                1
            },
            max_parallel_write: 1,
            max_background_threads: 0,
            max_spawn_fanout_per_turn: max_threads,
            max_agent_tokens_per_task: 200_000,
        }
    }

    fn hash(&self) -> Result<String> {
        hash_json(&json!({
            "max_threads": self.max_threads,
            "max_depth": self.max_depth,
            "max_parallel_readonly": self.max_parallel_readonly,
            "max_parallel_write": self.max_parallel_write,
            "max_background_threads": self.max_background_threads,
            "max_spawn_fanout_per_turn": self.max_spawn_fanout_per_turn,
            "max_agent_tokens_per_task": self.max_agent_tokens_per_task,
        }))
    }
}

/// Result of cancelling only the foreground parent run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ForegroundCancelImpact {
    pub foreground_children_interrupted: Vec<AgentInterruptedThread>,
    pub background_children_cancelled: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentInterruptedThread {
    pub thread_id: AgentThreadId,
    pub attempt_id: AgentRunAttemptId,
}

/// Runtime-owned supervisor for agent thread lifecycle, budget, and durable control entries.
#[derive(Debug, Clone)]
pub struct AgentSupervisor {
    registry: AgentProfileRegistry,
    budget: AgentBudgetPolicy,
    provider_capabilities: ProviderCapabilities,
    state: Arc<Mutex<AgentSupervisorState>>,
}

#[derive(Debug, Default)]
struct AgentSupervisorState {
    active_threads: BTreeMap<AgentThreadId, ActiveAgentThread>,
    spawn_fanout_this_turn: usize,
    task_token_usage: BTreeMap<TaskId, u64>,
}

#[derive(Debug, Clone)]
struct ActiveAgentThread {
    profile_id: AgentProfileId,
    attempt_id: AgentRunAttemptId,
    role: AgentRole,
    background: bool,
}

/// Runtime child runner that connects kernel task orchestration to the supervisor.
pub struct AgentSupervisorTaskChildRunner {
    supervisor: AgentSupervisor,
    subagent_read: BoxedAgent,
    subagent_write: BoxedAgent,
}

impl AgentSupervisorTaskChildRunner {
    pub fn new(
        supervisor: AgentSupervisor,
        subagent_read: BoxedAgent,
        subagent_write: BoxedAgent,
    ) -> Self {
        Self {
            supervisor,
            subagent_read,
            subagent_write,
        }
    }
}

impl AgentSupervisor {
    #[must_use]
    pub fn new(
        registry: AgentProfileRegistry,
        budget: AgentBudgetPolicy,
        provider_capabilities: ProviderCapabilities,
    ) -> Self {
        Self {
            registry,
            budget,
            provider_capabilities,
            state: Arc::new(Mutex::new(AgentSupervisorState::default())),
        }
    }

    #[must_use]
    pub fn registry(&self) -> &AgentProfileRegistry {
        &self.registry
    }

    #[must_use]
    pub fn budget(&self) -> &AgentBudgetPolicy {
        &self.budget
    }

    #[must_use]
    pub fn supports_background_resume(&self) -> bool {
        self.provider_capabilities.supports_agent_background_resume
    }

    #[must_use]
    pub fn cancel_foreground_run(&self) -> ForegroundCancelImpact {
        let foreground_children_interrupted = self
            .state
            .lock()
            .map(|mut state| {
                let thread_ids = state
                    .active_threads
                    .iter()
                    .filter(|(_, thread)| !thread.background)
                    .map(|(thread_id, _)| thread_id.clone())
                    .collect::<Vec<_>>();
                thread_ids
                    .into_iter()
                    .filter_map(|thread_id| {
                        state.active_threads.remove(&thread_id).map(|thread| {
                            AgentInterruptedThread {
                                thread_id,
                                attempt_id: thread.attempt_id,
                            }
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        ForegroundCancelImpact {
            foreground_children_interrupted,
            background_children_cancelled: 0,
        }
    }

    pub fn reset_turn_budget(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.spawn_fanout_this_turn = 0;
        }
    }

    #[must_use]
    pub fn active_profile_ids(&self) -> Vec<AgentProfileId> {
        self.state
            .lock()
            .map(|state| {
                state
                    .active_threads
                    .values()
                    .map(|thread| thread.profile_id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn begin_task_child_thread<H>(
        &self,
        session: &mut Session,
        handler: &mut H,
        start: AgentTaskChildStart,
    ) -> Result<AgentTaskChildThread>
    where
        H: EventHandler + Send,
    {
        let profile_id = profile_id_for_role(start.role)?;
        let resolved_profile = self
            .registry
            .get(&profile_id)
            .with_context(|| format!("agent profile {} is not registered", profile_id.as_str()))?;
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
        let attempt_id = AgentRunAttemptId::new(format!(
            "attempt_{}",
            short_digest(&hash_text(thread_id.as_str()))
        ))?;
        let prompt_hash = hash_child_input(&start.child_input)?;
        let provider_name = resolved_profile
            .profile
            .provider
            .clone()
            .unwrap_or_else(|| session.provider_name().to_owned());
        let model_name = resolved_profile
            .profile
            .model
            .clone()
            .unwrap_or_else(|| session.model_name().to_owned());
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
                parent_session_ref: start.parent_session_ref.clone(),
                thread_session_ref: start.child_session_ref.clone(),
                profile_id: profile_id.clone(),
                profile_snapshot_id: snapshot.snapshot_id.clone(),
                run_context,
                objective: start.objective.clone(),
                prompt_hash,
                invocation_mode: start.invocation_mode,
                invocation_source: start.invocation_source,
                display_name: start.step.display_name.clone(),
                created_at_ms: None,
            }),
        )?;

        if start.role == AgentRole::SubagentWrite
            && tool_scope_is_write_capable(&resolved_profile.profile.tool_scope)
        {
            let reason = "write-capable agents require changeset or path lease support".to_owned();
            append_control(
                session,
                handler,
                ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
                    thread_id,
                    status: AgentThreadStatus::Failed,
                    reason: Some(reason),
                    updated_at_ms: None,
                }),
            )?;
            bail!("write-capable agent requires changeset or path lease support");
        }

        if let Err(reason) = self.reserve_thread(
            &thread_id,
            &attempt_id,
            &profile_id,
            &start.task_id,
            start.role,
            start.invocation_mode,
            start.parent_depth,
        ) {
            append_control(
                session,
                handler,
                ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
                    thread_id,
                    status: AgentThreadStatus::Failed,
                    reason: Some(reason),
                    updated_at_ms: None,
                }),
            )?;
            bail!("agent budget denied child session");
        }

        append_control(
            session,
            handler,
            ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
                thread_id: thread_id.clone(),
                status: AgentThreadStatus::Running,
                reason: Some("child session started".to_owned()),
                updated_at_ms: None,
            }),
        )?;
        append_control(
            session,
            handler,
            ControlEntry::AgentRunAttemptStarted(AgentRunAttemptStartedEntry {
                thread_id: thread_id.clone(),
                attempt_id: attempt_id.clone(),
                provider: provider_name,
                model: model_name,
                background: matches!(start.invocation_mode, AgentInvocationMode::Background),
                provider_background_handle_ref: None,
            }),
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
        let attempt_id = AgentRunAttemptId::new(format!(
            "attempt_{}",
            short_digest(&hash_text(thread_id.as_str()))
        ))?;
        let prompt_hash = hash_text(&start.prompt);
        let provider_name = resolved_profile
            .profile
            .provider
            .clone()
            .unwrap_or_else(|| session.provider_name().to_owned());
        let model_name = resolved_profile
            .profile
            .model
            .clone()
            .unwrap_or_else(|| session.model_name().to_owned());
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
                parent_session_ref: start.parent_session_ref.clone(),
                thread_session_ref: start.child_session_ref.clone(),
                profile_id: start.profile_id.clone(),
                profile_snapshot_id: snapshot.snapshot_id.clone(),
                run_context,
                objective: start.objective.clone(),
                prompt_hash,
                invocation_mode: start.invocation_mode,
                invocation_source: start.invocation_source,
                display_name: start.display_name_hint.clone(),
                created_at_ms: None,
            }),
        )?;

        if start.role == AgentRole::SubagentWrite
            && tool_scope_is_write_capable(&resolved_profile.profile.tool_scope)
        {
            let reason = "write-capable agents require changeset or path lease support".to_owned();
            append_control(
                session,
                handler,
                ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
                    thread_id,
                    status: AgentThreadStatus::Failed,
                    reason: Some(reason),
                    updated_at_ms: None,
                }),
            )?;
            bail!("write-capable agent requires changeset or path lease support");
        }

        if let Err(reason) = self.reserve_thread(
            &thread_id,
            &attempt_id,
            &start.profile_id,
            &start.budget_scope_id,
            start.role,
            start.invocation_mode,
            start.parent_depth,
        ) {
            append_control(
                session,
                handler,
                ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
                    thread_id,
                    status: AgentThreadStatus::Failed,
                    reason: Some(reason),
                    updated_at_ms: None,
                }),
            )?;
            bail!("agent budget denied child session");
        }

        append_control(
            session,
            handler,
            ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
                thread_id: thread_id.clone(),
                status: AgentThreadStatus::Running,
                reason: Some("agent tool spawned child session".to_owned()),
                updated_at_ms: None,
            }),
        )?;
        append_control(
            session,
            handler,
            ControlEntry::AgentRunAttemptStarted(AgentRunAttemptStartedEntry {
                thread_id: thread_id.clone(),
                attempt_id: attempt_id.clone(),
                provider: provider_name,
                model: model_name,
                background: matches!(start.invocation_mode, AgentInvocationMode::Background),
                provider_background_handle_ref: None,
            }),
        )?;
        Ok(AgentChatChildThread {
            thread_id,
            attempt_id,
            profile_id: start.profile_id,
            parent_thread_id: start.parent_thread_id,
            child_session_ref: start.child_session_ref,
            budget_scope_id: start.budget_scope_id,
        })
    }

    pub fn validate_usage_budget(&self, task_id: &TaskId, usage: &AgentUsageSummary) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("agent supervisor state lock poisoned"))?;
        let current_tokens = *state.task_token_usage.get(task_id).unwrap_or(&0);
        let total_tokens = current_tokens.saturating_add(usage.total_tokens);
        state.task_token_usage.insert(task_id.clone(), total_tokens);
        if total_tokens > self.budget.max_agent_tokens_per_task {
            bail!(
                "agent token budget exceeded: task_id={} total_tokens={} max_agent_tokens_per_task={}",
                task_id.as_str(),
                total_tokens,
                self.budget.max_agent_tokens_per_task
            );
        }
        Ok(())
    }

    pub fn record_task_child_result<H>(
        &self,
        session: &mut Session,
        handler: &mut H,
        thread: &AgentTaskChildThread,
        child_session_ref: SessionRef,
        status: TaskChildSessionStatus,
        final_text: &str,
        outcome: &sigil_kernel::AgentRunOutcome,
        usage: Option<AgentUsageSummary>,
    ) -> Result<()>
    where
        H: EventHandler + Send,
    {
        let terminal_status = agent_terminal_status_from_task_child(status);
        let summary = bounded_agent_summary(final_text);
        append_control(
            session,
            handler,
            ControlEntry::AgentThreadResultRecorded(AgentThreadResultRecordedEntry {
                result: AgentThreadResult {
                    thread_id: thread.thread_id.clone(),
                    session_ref: child_session_ref.clone(),
                    status: terminal_status,
                    summary: summary.text,
                    summary_truncated: summary.truncated,
                    original_summary_chars: summary.original_chars,
                    artifacts: agent_result_artifacts(&child_session_ref, final_text),
                    changed_paths: outcome.changed_files.clone(),
                    risks: Vec::new(),
                    followups: Vec::new(),
                    usage,
                    output_hash: hash_text(final_text),
                },
            }),
        )?;
        append_control(
            session,
            handler,
            ControlEntry::AgentMergeSafePoint(AgentMergeSafePointEntry {
                thread_id: thread.thread_id.clone(),
                parent_thread_id: thread.parent_thread_id.clone(),
                result_hash: hash_text(final_text),
            }),
        )?;
        self.release_thread(&thread.thread_id);
        Ok(())
    }

    pub fn record_chat_child_result<H>(
        &self,
        session: &mut Session,
        handler: &mut H,
        thread: &AgentChatChildThread,
        status: TaskChildSessionStatus,
        final_text: &str,
        outcome: &sigil_kernel::AgentRunOutcome,
        usage: Option<AgentUsageSummary>,
    ) -> Result<()>
    where
        H: EventHandler + Send + ?Sized,
    {
        let terminal_status = agent_terminal_status_from_task_child(status);
        let summary = bounded_agent_summary(final_text);
        append_control(
            session,
            handler,
            ControlEntry::AgentThreadResultRecorded(AgentThreadResultRecordedEntry {
                result: AgentThreadResult {
                    thread_id: thread.thread_id.clone(),
                    session_ref: thread.child_session_ref.clone(),
                    status: terminal_status,
                    summary: summary.text,
                    summary_truncated: summary.truncated,
                    original_summary_chars: summary.original_chars,
                    artifacts: agent_result_artifacts(&thread.child_session_ref, final_text),
                    changed_paths: outcome.changed_files.clone(),
                    risks: Vec::new(),
                    followups: Vec::new(),
                    usage,
                    output_hash: hash_text(final_text),
                },
            }),
        )?;
        append_control(
            session,
            handler,
            ControlEntry::AgentMergeSafePoint(AgentMergeSafePointEntry {
                thread_id: thread.thread_id.clone(),
                parent_thread_id: thread.parent_thread_id.clone(),
                result_hash: hash_text(final_text),
            }),
        )?;
        self.release_thread(&thread.thread_id);
        Ok(())
    }

    pub fn record_task_child_failure<H>(
        &self,
        session: &mut Session,
        handler: &mut H,
        thread: &AgentTaskChildThread,
        reason: String,
    ) -> Result<()>
    where
        H: EventHandler + Send,
    {
        append_control(
            session,
            handler,
            ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
                thread_id: thread.thread_id.clone(),
                status: AgentThreadStatus::Failed,
                reason: Some(reason),
                updated_at_ms: None,
            }),
        )?;
        self.release_thread(&thread.thread_id);
        Ok(())
    }

    pub fn record_chat_child_failure<H>(
        &self,
        session: &mut Session,
        handler: &mut H,
        thread: &AgentChatChildThread,
        reason: String,
    ) -> Result<()>
    where
        H: EventHandler + Send + ?Sized,
    {
        append_control(
            session,
            handler,
            ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
                thread_id: thread.thread_id.clone(),
                status: AgentThreadStatus::Failed,
                reason: Some(reason),
                updated_at_ms: None,
            }),
        )?;
        self.release_thread(&thread.thread_id);
        Ok(())
    }

    fn reserve_thread(
        &self,
        thread_id: &AgentThreadId,
        attempt_id: &AgentRunAttemptId,
        profile_id: &AgentProfileId,
        task_id: &TaskId,
        role: AgentRole,
        invocation_mode: AgentInvocationMode,
        parent_depth: usize,
    ) -> std::result::Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "agent supervisor state lock poisoned".to_owned())?;
        if state.active_threads.len() >= self.budget.max_threads {
            return Err(format!(
                "agent thread budget exceeded: max_threads={}",
                self.budget.max_threads
            ));
        }
        if parent_depth >= self.budget.max_depth {
            return Err(format!(
                "agent depth budget exceeded: max_depth={}",
                self.budget.max_depth
            ));
        }
        let current_task_tokens = *state.task_token_usage.get(task_id).unwrap_or(&0);
        if current_task_tokens >= self.budget.max_agent_tokens_per_task {
            return Err(format!(
                "agent token budget exceeded before spawn: task_id={} total_tokens={} max_agent_tokens_per_task={}",
                task_id.as_str(),
                current_task_tokens,
                self.budget.max_agent_tokens_per_task
            ));
        }
        if state.spawn_fanout_this_turn >= self.budget.max_spawn_fanout_per_turn {
            return Err(format!(
                "agent fan-out budget exceeded: max_spawn_fanout_per_turn={}",
                self.budget.max_spawn_fanout_per_turn
            ));
        }
        let background_count = state
            .active_threads
            .values()
            .filter(|thread| thread.background)
            .count();
        if matches!(invocation_mode, AgentInvocationMode::Background)
            && background_count >= self.budget.max_background_threads
        {
            return Err(format!(
                "background agent budget exceeded: max_background_threads={}",
                self.budget.max_background_threads
            ));
        }
        let readonly_count = state
            .active_threads
            .values()
            .filter(|thread| thread.role == AgentRole::SubagentRead)
            .count();
        if role == AgentRole::SubagentRead && readonly_count >= self.budget.max_parallel_readonly {
            return Err(format!(
                "readonly agent budget exceeded: max_parallel_readonly={}",
                self.budget.max_parallel_readonly
            ));
        }
        let write_count = state
            .active_threads
            .values()
            .filter(|thread| thread.role == AgentRole::SubagentWrite)
            .count();
        if role == AgentRole::SubagentWrite && write_count >= self.budget.max_parallel_write {
            return Err(format!(
                "write agent budget exceeded: max_parallel_write={}",
                self.budget.max_parallel_write
            ));
        }
        if role == AgentRole::SubagentWrite
            && matches!(invocation_mode, AgentInvocationMode::Background)
        {
            return Err("background write agents are disabled".to_owned());
        }
        state.spawn_fanout_this_turn += 1;
        state.active_threads.insert(
            thread_id.clone(),
            ActiveAgentThread {
                profile_id: profile_id.clone(),
                attempt_id: attempt_id.clone(),
                role,
                background: matches!(invocation_mode, AgentInvocationMode::Background),
            },
        );
        Ok(())
    }

    fn release_thread(&self, thread_id: &AgentThreadId) {
        if let Ok(mut state) = self.state.lock() {
            state.active_threads.remove(thread_id);
        }
    }

    pub(crate) fn release_runtime_thread(&self, thread_id: &AgentThreadId) {
        self.release_thread(thread_id);
    }

    pub fn append_foreground_cancel_audit<H>(
        session: &mut Session,
        handler: &mut H,
        impact: ForegroundCancelImpact,
        reason: &str,
    ) -> Result<()>
    where
        H: EventHandler + Send + ?Sized,
    {
        for interrupted in impact.foreground_children_interrupted {
            append_control(
                session,
                handler,
                ControlEntry::AgentRunInterrupted(AgentRunInterruptedEntry {
                    thread_id: interrupted.thread_id.clone(),
                    attempt_id: interrupted.attempt_id,
                    reason: reason.to_owned(),
                }),
            )?;
            append_control(
                session,
                handler,
                ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
                    thread_id: interrupted.thread_id,
                    status: AgentThreadStatus::Interrupted,
                    reason: Some(reason.to_owned()),
                    updated_at_ms: None,
                }),
            )?;
        }
        Ok(())
    }
}

struct BoundedAgentSummary {
    text: String,
    truncated: bool,
    original_chars: Option<usize>,
}

fn bounded_agent_summary(final_text: &str) -> BoundedAgentSummary {
    let trimmed = final_text.trim();
    let original_chars = trimmed.chars().count();
    let text = trimmed
        .chars()
        .take(AGENT_RESULT_SUMMARY_LIMIT)
        .collect::<String>();
    let rendered_chars = text.chars().count();
    let truncated = original_chars > rendered_chars;
    BoundedAgentSummary {
        text,
        truncated,
        original_chars: truncated.then_some(original_chars),
    }
}

fn agent_result_artifacts(
    child_session_ref: &SessionRef,
    final_text: &str,
) -> Vec<AgentArtifactRef> {
    vec![AgentArtifactRef {
        kind: "child_session".to_owned(),
        path: child_session_ref.as_path().display().to_string(),
        hash: Some(hash_text(final_text)),
    }]
}

#[derive(Debug, Clone)]
pub struct AgentTaskChildStart {
    pub task_id: TaskId,
    pub parent_thread_id: AgentThreadId,
    pub parent_depth: usize,
    pub parent_session_ref: SessionRef,
    pub plan_version: u32,
    pub step: TaskStepSpec,
    pub child_task_id: TaskId,
    pub child_session_ref: SessionRef,
    pub child_input: AgentRunInput,
    pub objective: String,
    pub workspace_root: std::path::PathBuf,
    pub provider_capabilities: ProviderCapabilities,
    pub role: AgentRole,
    pub invocation_mode: AgentInvocationMode,
    pub invocation_source: AgentInvocationSource,
}

#[derive(Debug, Clone)]
pub struct AgentTaskChildThread {
    pub thread_id: AgentThreadId,
    pub attempt_id: AgentRunAttemptId,
    pub profile_id: AgentProfileId,
    pub parent_thread_id: AgentThreadId,
}

#[derive(Debug, Clone)]
pub struct AgentChatChildStart {
    pub call_id: String,
    pub budget_scope_id: TaskId,
    pub parent_thread_id: AgentThreadId,
    pub parent_depth: usize,
    pub parent_session_ref: SessionRef,
    pub profile_id: AgentProfileId,
    pub role: AgentRole,
    pub child_session_ref: SessionRef,
    pub objective: String,
    pub prompt: String,
    pub workspace_root: std::path::PathBuf,
    pub provider_capabilities: ProviderCapabilities,
    pub invocation_mode: AgentInvocationMode,
    pub invocation_source: AgentInvocationSource,
    pub display_name_hint: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AgentChatChildThread {
    pub thread_id: AgentThreadId,
    pub attempt_id: AgentRunAttemptId,
    pub profile_id: AgentProfileId,
    pub parent_thread_id: AgentThreadId,
    pub child_session_ref: SessionRef,
    pub budget_scope_id: TaskId,
}

#[async_trait]
impl TaskChildSessionRunner for AgentSupervisorTaskChildRunner {
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
        if !matches!(
            request.step.role,
            AgentRole::SubagentRead | AgentRole::SubagentWrite
        ) {
            bail!("supervisor child runner requires a subagent role");
        }
        let child_task_id = TaskId::new(format!(
            "child_v{}_{}",
            request.plan_version,
            request.step.step_id.as_str()
        ))?;
        let child_session_ref =
            child_session_ref(&request.task.task_id, &request.step.step_id, &child_task_id)?;
        let agent = match request.step.role {
            AgentRole::SubagentRead => &self.subagent_read,
            AgentRole::SubagentWrite => &self.subagent_write,
            AgentRole::Planner | AgentRole::Executor => unreachable!("role checked above"),
        };
        let child_thread = self.supervisor.begin_task_child_thread(
            parent_session,
            handler,
            AgentTaskChildStart {
                task_id: request.task.task_id.clone(),
                parent_thread_id: main_thread_id()?,
                parent_depth: 0,
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
            },
        )?;
        append_task_child_session(
            parent_session,
            handler,
            &request,
            &child_task_id,
            &child_session_ref,
            TaskChildSessionStatus::Started,
            None,
        )?;
        let mut child_session = build_child_session(parent_session, &child_session_ref)?;
        let mut route_handler = SupervisorTaskApprovalRouteHandler {
            inner: approval_handler,
            parent_session,
            task_request: &request,
            child_session_ref: &child_session_ref,
            source_thread_id: &child_thread.thread_id,
        };
        let child_input = request.child_input.clone();
        let options = request.options.clone();
        let output = match agent
            .run_with_approval_input(
                &mut child_session,
                child_input,
                options,
                handler,
                &mut route_handler,
            )
            .await
        {
            Ok(output) => output,
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
                return Err(error);
            }
        };
        let final_text = output.result.final_text;
        let outcome = output.outcome;
        let usage = usage_summary_from_stats(child_session.stats());
        let budget_warning = self
            .supervisor
            .validate_usage_budget(&request.task.task_id, &usage)
            .err()
            .map(|error| format!("{error:#}"));
        let status = task_child_status_from_outcome(&final_text, &outcome);
        append_task_child_session(
            route_handler.parent_session,
            handler,
            &request,
            &child_task_id,
            &child_session_ref,
            status,
            Some(hash_text(&final_text)),
        )?;
        self.supervisor.record_task_child_result(
            route_handler.parent_session,
            handler,
            &child_thread,
            child_session_ref.clone(),
            status,
            &final_text,
            &outcome,
            Some(usage),
        )?;
        if let Some(warning) = budget_warning {
            let _ = handler.handle(RunEvent::Notice(format!(
                "agent budget warning after child completion: {warning}"
            )));
        }
        Ok(TaskChildSessionRunOutput {
            final_text,
            outcome,
        })
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
            ToolApproval::Approve => (TaskRouteStatus::Resolved, AgentRouteStatus::Resolved),
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

fn append_control<H>(session: &mut Session, handler: &mut H, control: ControlEntry) -> Result<()>
where
    H: EventHandler + Send + ?Sized,
{
    session.append_control(control.clone())?;
    handler.handle(RunEvent::Control(control))
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
    H: EventHandler + Send,
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
    session.append_control(ControlEntry::TaskSubagentApprovalRoute(
        TaskSubagentApprovalRouteEntry {
            route_id: route_id.clone(),
            task_id: request.task.task_id.clone(),
            plan_version: request.plan_version,
            step_id: request.step.step_id.clone(),
            role: request.step.role,
            child_session_ref: child_session_ref.clone(),
            call_id: call.id.clone(),
            tool_name: call.name.clone(),
            status,
        },
    ))
}

fn build_child_session(
    parent_session: &Session,
    child_session_ref: &SessionRef,
) -> Result<Session> {
    if let Some(parent_path) = parent_session.store_path() {
        let parent_dir = parent_path.parent().unwrap_or_else(|| Path::new("."));
        let store = JsonlSessionStore::new(child_session_ref.resolve(parent_dir))?;
        return Session::load_from_store(
            parent_session.provider_name(),
            parent_session.model_name(),
            store,
        );
    }
    Ok(Session::new(
        parent_session.provider_name(),
        parent_session.model_name(),
    ))
}

fn profile_id_for_role(role: AgentRole) -> Result<AgentProfileId> {
    match role {
        AgentRole::SubagentRead => AgentProfileId::new(EXPLORE_PROFILE_ID),
        AgentRole::SubagentWrite => AgentProfileId::new(WORKER_PROFILE_ID),
        AgentRole::Planner | AgentRole::Executor => Err(anyhow!(
            "agent supervisor child profile requires a subagent role"
        )),
    }
}

fn agent_thread_id_for_task_child(
    task_id: &TaskId,
    plan_version: u32,
    step: &TaskStepSpec,
    child_task_id: &TaskId,
) -> Result<AgentThreadId> {
    let hash = hash_text(&format!(
        "{}:{}:{}:{}",
        task_id.as_str(),
        plan_version,
        step.step_id.as_str(),
        child_task_id.as_str()
    ));
    let digest = short_digest(&hash);
    AgentThreadId::new(format!("agent_v{plan_version}_{}", digest))
}

pub fn chat_agent_thread_id_for_call(
    call_id: &str,
    profile_id: &AgentProfileId,
) -> Result<AgentThreadId> {
    let hash = hash_text(&format!("chat:{}:{}", call_id, profile_id.as_str()));
    AgentThreadId::new(format!("agent_chat_{}", short_digest(&hash)))
}

fn task_route_id_for_call(
    task_id: &TaskId,
    step_id: &sigil_kernel::TaskStepId,
    call_id: &str,
) -> Result<TaskRouteId> {
    let digest = hash_text(&format!(
        "{}:{}:{}",
        task_id.as_str(),
        step_id.as_str(),
        call_id
    ));
    TaskRouteId::new(format!("route_{}", short_digest(&digest)))
}

fn agent_route_id_for_call(thread_id: &AgentThreadId, call_id: &str) -> Result<AgentRouteId> {
    let digest = hash_text(&format!("{}:{}", thread_id.as_str(), call_id));
    AgentRouteId::new(format!("agent_route_{}", short_digest(&digest)))
}

fn task_child_status_from_outcome(
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

fn agent_terminal_status_from_task_child(
    status: TaskChildSessionStatus,
) -> AgentThreadTerminalStatus {
    match status {
        TaskChildSessionStatus::Completed => AgentThreadTerminalStatus::Completed,
        TaskChildSessionStatus::Failed | TaskChildSessionStatus::Unavailable => {
            AgentThreadTerminalStatus::Failed
        }
        TaskChildSessionStatus::Cancelled => AgentThreadTerminalStatus::Cancelled,
        TaskChildSessionStatus::Interrupted | TaskChildSessionStatus::Started => {
            AgentThreadTerminalStatus::Interrupted
        }
    }
}

fn child_provider_capabilities(agent: &BoxedAgent) -> ProviderCapabilities {
    agent.provider_capabilities()
}

fn usage_summary_from_stats(stats: &SessionStats) -> AgentUsageSummary {
    let input_tokens = stats.prompt_tokens;
    let output_tokens = stats.completion_tokens;
    AgentUsageSummary {
        input_tokens,
        output_tokens,
        total_tokens: input_tokens + output_tokens,
        cached_tokens: Some(stats.cache_hit_tokens),
    }
}

fn tool_scope_is_write_capable(scope: &ToolRegistryScope) -> bool {
    scope.allow_all
        || WRITE_CAPABLE_TOOL_NAMES
            .iter()
            .any(|tool_name| scope.allows(tool_name))
        || scope.prefixes.iter().any(|prefix| {
            WRITE_CAPABLE_TOOL_PREFIXES
                .iter()
                .any(|write_prefix| prefix.starts_with(write_prefix))
        })
}

const WRITE_CAPABLE_TOOL_NAMES: &[&str] = &["write_file", "edit_file", "apply_changeset", "bash"];
const WRITE_CAPABLE_TOOL_PREFIXES: &[&str] = &["mcp__"];

fn main_thread_id() -> Result<AgentThreadId> {
    AgentThreadId::new("main")
}

fn hash_child_input(input: &AgentRunInput) -> Result<String> {
    hash_json(&json!({
        "persisted_user_message": input.persisted_user_message.as_deref(),
        "transient_context": &input.transient_context,
        "task_plan_update": input.task_plan_update.as_ref().map(|context| {
            json!({
                "task_id": context.task_id.as_str(),
                "max_plan_steps": context.max_plan_steps,
            })
        }),
    }))
}

fn hash_provider_capabilities(capabilities: &ProviderCapabilities) -> Result<String> {
    hash_json(&serde_json::to_value(capabilities)?)
}

fn hash_json(value: &Value) -> Result<String> {
    let bytes = serde_json::to_vec(value)?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

fn hash_text(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn short_digest(hash: &str) -> &str {
    hash.get(..12).unwrap_or(hash)
}

#[cfg(test)]
#[path = "tests/agent_supervisor_tests.rs"]
mod tests;
