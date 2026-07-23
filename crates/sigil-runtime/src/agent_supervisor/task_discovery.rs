use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use sigil_kernel::{
    AgentInvocationMode, AgentInvocationSource, AgentProfileSource, AgentRole, AgentRouteId,
    AgentRunInput, AgentRunOptions, AgentThreadId, AgentTrustState, ApprovalHandler, ApprovalMode,
    EventHandler, ModelMessage, RunCancellationHandle, RunEvent, SequentialTaskRequest, Session,
    SessionRef, TaskChildSessionStatus, TaskId, TaskIsolationMode, TaskParticipantAttemptId,
    TaskStepId, TaskStepMode, TaskStepSpec, Tool, ToolAccess, ToolApproval, ToolCall, ToolCategory,
    ToolContext, ToolErrorKind, ToolPreviewCapability, ToolRegistry, ToolResult, ToolResultMeta,
    ToolSpec, ToolSubject, WebTaskTreeBudget, child_session_ref,
};

use crate::{
    EXPLORE_PROFILE_ID,
    agent_completion::{AgentCompletionHub, AgentCompletionRegistration},
    agent_tools::tool_registry_is_safe_readonly_for_auto_spawn,
};

use super::{
    AgentResultMaterialization, AgentSupervisor, AgentTaskChildStart, AgentTaskChildThread,
    BoxedAgent, hash_text, materialize_child_agent_final_answer, short_digest,
    task_runner::{build_child_session, task_child_status_from_outcome, usage_summary_from_stats},
};

pub const REQUEST_TASK_DISCOVERY_TOOL_NAME: &str = "request_task_discovery";
pub const MAX_TASK_DISCOVERY_PROBES: usize = 4;
const MAX_DISCOVERY_PATH_HINTS: usize = 16;
const MAX_DISCOVERY_TITLE_CHARS: usize = 160;
const MAX_DISCOVERY_OBJECTIVE_CHARS: usize = 2_000;
const MAX_DISCOVERY_PATH_CHARS: usize = 512;

pub(super) fn planner_tools_with_discovery(base: &ToolRegistry, max_probes: usize) -> ToolRegistry {
    let mut registry = base.snapshot();
    registry.register(Arc::new(TaskDiscoveryTool {
        max_probes: max_probes.min(MAX_TASK_DISCOVERY_PROBES),
    }));
    registry
}

pub(super) struct TaskDiscoveryDelegate<'a> {
    supervisor: AgentSupervisor,
    parent_session: &'a mut Session,
    task: SequentialTaskRequest,
    planner_attempt_id: TaskParticipantAttemptId,
    planner_thread_id: AgentThreadId,
    explore_agent: Arc<BoxedAgent>,
    options: AgentRunOptions,
    max_probes: usize,
    requested: bool,
    cancellation: Option<RunCancellationHandle>,
    web_task_tree_budget: Option<Arc<WebTaskTreeBudget>>,
}

impl<'a> TaskDiscoveryDelegate<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        supervisor: AgentSupervisor,
        parent_session: &'a mut Session,
        task: SequentialTaskRequest,
        planner_attempt_id: TaskParticipantAttemptId,
        planner_thread_id: AgentThreadId,
        explore_agent: Arc<BoxedAgent>,
        options: AgentRunOptions,
        max_probes: usize,
    ) -> Self {
        Self {
            supervisor,
            parent_session,
            task,
            planner_attempt_id,
            planner_thread_id,
            explore_agent,
            options,
            max_probes: max_probes.min(MAX_TASK_DISCOVERY_PROBES),
            requested: false,
            cancellation: None,
            web_task_tree_budget: None,
        }
    }

    async fn run_discovery(
        &mut self,
        call: &ToolCall,
        args: &Value,
        handler: &mut (dyn EventHandler + Send),
    ) -> Result<ToolResult> {
        if self.max_probes == 0 {
            return Ok(discovery_error(
                call,
                ToolErrorKind::PermissionDenied,
                "planner discovery is disabled by runtime policy",
            ));
        }
        if self.requested {
            return Ok(discovery_error(
                call,
                ToolErrorKind::PermissionDenied,
                "request_task_discovery may be called at most once per planning attempt",
            ));
        }
        self.requested = true;

        let probes = match parse_task_discovery_probes(args, self.max_probes) {
            Ok(probes) => probes,
            Err(error) => {
                return Ok(discovery_error(
                    call,
                    ToolErrorKind::InvalidInput,
                    &format!("{error:#}"),
                ));
            }
        };
        let cancellation = match self.cancellation.clone() {
            Some(cancellation) if !cancellation.is_cancel_requested() => cancellation,
            Some(_) => {
                return Ok(discovery_error(
                    call,
                    ToolErrorKind::Interrupted,
                    "root run cancelled before planner discovery admission",
                ));
            }
            None => {
                return Ok(discovery_error(
                    call,
                    ToolErrorKind::Internal,
                    "planner discovery requires the current root cancellation scope",
                ));
            }
        };
        let explore_profile_id = sigil_kernel::AgentProfileId::new(EXPLORE_PROFILE_ID)?;
        let resolved_profile = self
            .supervisor
            .registry()
            .get(&explore_profile_id)
            .ok_or_else(|| anyhow!("trusted built-in explore profile is not registered"))?;
        if resolved_profile.source != AgentProfileSource::System
            || resolved_profile.trust_state != AgentTrustState::Trusted
            || !resolved_profile.effective_enabled()
            || resolved_profile.execution_role != AgentRole::SubagentRead
        {
            return Ok(discovery_error(
                call,
                ToolErrorKind::PermissionDenied,
                "planner discovery requires the enabled trusted built-in explore profile",
            ));
        }
        if !tool_registry_is_safe_readonly_for_auto_spawn(self.explore_agent.tool_registry()) {
            return Ok(discovery_error(
                call,
                ToolErrorKind::PermissionDenied,
                "planner discovery Explore contracts are not proven read-only",
            ));
        }

        let mut prepared = Vec::with_capacity(probes.len());
        for (sequence, probe) in probes.into_iter().enumerate() {
            let step_id = TaskStepId::new(format!("discovery-{}", probe.probe_id.as_str()))?;
            let child_task_id = discovery_child_task_id(
                &self.task.task_id,
                &self.planner_attempt_id,
                &probe.probe_id,
            )?;
            let child_session_ref =
                child_session_ref(&self.task.task_id, &step_id, &child_task_id)?;
            let mut child_input = AgentRunInput::without_persisted_user_message(vec![
                ModelMessage::system(
                    "You are a read-only planner discovery probe. Investigate only the assigned objective and path hints. Do not modify files, spawn agents, create plans, or poll background work. Return concise factual findings, uncertainties, and relevant paths.",
                ),
                ModelMessage::user(discovery_probe_prompt(&self.task.objective, &probe)),
            ])
            .with_child_cancellation(cancellation.clone())
            .with_logical_run_id(discovery_logical_run_id(
                &self.planner_attempt_id,
                &probe.probe_id,
            ));
            if let Some(budget) = &self.web_task_tree_budget {
                child_input = child_input.with_web_task_tree_budget(Arc::clone(budget));
            }
            let child_session = build_child_session(self.parent_session, &child_session_ref)?;
            let step = TaskStepSpec {
                step_id,
                title: probe.title.clone(),
                display_name: None,
                detail: Some(probe.objective.clone()),
                role: AgentRole::SubagentRead,
                depends_on: Vec::new(),
                mode: Some(TaskStepMode::Read),
                isolation: Some(TaskIsolationMode::SharedReadOnly),
            };
            let start = AgentTaskChildStart {
                task_id: self.task.task_id.clone(),
                parent_thread_id: self.planner_thread_id.clone(),
                parent_depth: 1,
                parent_session_ref: self.task.parent_session_ref.clone(),
                plan_version: 0,
                step,
                child_task_id,
                child_session_ref: child_session_ref.clone(),
                child_input: child_input.clone(),
                objective: probe.objective.clone(),
                workspace_root: self.options.workspace_root.clone(),
                provider_capabilities: self.explore_agent.provider_capabilities(),
                role: AgentRole::SubagentRead,
                invocation_mode: AgentInvocationMode::JoinBeforeFinal,
                invocation_source: AgentInvocationSource::Task,
            };
            prepared.push(PreparedDiscoveryProbe {
                sequence: u64::try_from(sequence)
                    .map_err(|_| anyhow!("planner discovery sequence overflowed"))?,
                probe,
                start,
                child_session,
                child_session_ref,
                child_input,
            });
        }

        let starts = prepared
            .iter()
            .map(|prepared| prepared.start.clone())
            .collect::<Vec<_>>();
        let reservation = match self.supervisor.reserve_task_child_batch(&starts) {
            Ok(reservation) => reservation,
            Err(error) => {
                return Ok(discovery_error(
                    call,
                    ToolErrorKind::PermissionDenied,
                    &format!("{error:#}"),
                ));
            }
        };
        let mut started: Vec<StartedDiscoveryProbe> = Vec::with_capacity(prepared.len());
        for prepared_probe in prepared {
            let thread = match self.supervisor.begin_task_child_thread(
                self.parent_session,
                handler,
                prepared_probe.start,
            ) {
                Ok(thread) => thread,
                Err(error) => {
                    for started_probe in &started {
                        let _ = self.supervisor.record_task_child_failure(
                            self.parent_session,
                            handler,
                            &started_probe.thread,
                            "planner discovery start rolled back before provider dispatch"
                                .to_owned(),
                        );
                    }
                    return Ok(discovery_error(
                        call,
                        ToolErrorKind::Internal,
                        &format!("failed to commit planner discovery child start: {error:#}"),
                    ));
                }
            };
            started.push(StartedDiscoveryProbe {
                sequence: prepared_probe.sequence,
                probe: prepared_probe.probe,
                thread,
                child_session: prepared_probe.child_session,
                child_session_ref: prepared_probe.child_session_ref,
                child_input: prepared_probe.child_input,
            });
        }
        reservation.commit();

        let mut registrations = Vec::with_capacity(started.len());
        for started_probe in started {
            let key = (
                started_probe.thread.thread_id.clone(),
                started_probe.thread.attempt_id.clone(),
            );
            let sequence = started_probe.sequence;
            let context = DiscoveryCompletionContext {
                probe: started_probe.probe,
                thread: started_probe.thread.clone(),
                _release_guard: DiscoveryThreadReleaseGuard::new(
                    &self.supervisor,
                    &started_probe.thread,
                ),
            };
            let agent = Arc::clone(&self.explore_agent);
            let options = self.options.clone();
            let future = run_discovery_probe(
                agent,
                started_probe.thread,
                started_probe.child_session,
                started_probe.child_session_ref,
                started_probe.child_input,
                options,
            );
            registrations.push(AgentCompletionRegistration::new(
                key, sequence, context, future,
            ));
        }
        let completion_hub = match AgentCompletionHub::from_batch(registrations) {
            Ok(hub) => hub,
            Err(rejection) => {
                let (error, registrations) = rejection.into_parts();
                let reason = format!("planner discovery completion batch rejected: {error}");
                let mut first_cleanup_error = None;
                for registration in registrations {
                    let (_key, _sequence, context, future) = registration.into_parts();
                    drop(future);
                    let result = self.supervisor.record_task_child_failure(
                        self.parent_session,
                        handler,
                        &context.thread,
                        reason.clone(),
                    );
                    if first_cleanup_error.is_none() {
                        first_cleanup_error = result.err();
                    }
                }
                if let Some(cleanup_error) = first_cleanup_error {
                    return Err(anyhow!(reason).context(format!(
                        "planner discovery completion cleanup also failed: {cleanup_error:#}"
                    )));
                }
                return Err(anyhow!(error));
            }
        };
        let completions = completion_hub.collect().await;
        let mut members = Vec::with_capacity(completions.len());
        let mut first_commit_error = None;
        for completion in completions {
            let sequence = completion.sequence;
            let DiscoveryCompletionContext {
                probe,
                thread,
                _release_guard,
            } = completion.context;
            let member = match completion.result {
                Ok(output) => {
                    let commit = self.supervisor.record_task_child_result(
                        self.parent_session,
                        handler,
                        &thread,
                        output.child_session_ref,
                        output.status,
                        &output.materialized,
                        &output.outcome,
                        Some(output.usage),
                    );
                    if let Err(error) = commit {
                        if first_commit_error.is_none() {
                            first_commit_error = Some(error.context(format!(
                                "failed to commit planner discovery probe {}",
                                probe.probe_id.as_str()
                            )));
                        }
                        discovery_member_error(&probe, &thread, "failed to commit discovery result")
                    } else {
                        discovery_member_from_projection(self.parent_session, &probe, &thread)
                    }
                }
                Err(error) => {
                    let reason = format!("{error:#}");
                    if let Err(commit_error) = self.supervisor.record_task_child_failure(
                        self.parent_session,
                        handler,
                        &thread,
                        reason.clone(),
                    ) && first_commit_error.is_none()
                    {
                        first_commit_error = Some(commit_error.context(format!(
                            "failed to commit planner discovery failure {}",
                            probe.probe_id.as_str()
                        )));
                    }
                    discovery_member_error(&probe, &thread, &reason)
                }
            };
            members.push((sequence, member));
        }
        if let Some(error) = first_commit_error {
            return Err(error);
        }
        members.sort_by_key(|(sequence, _)| *sequence);
        let members = members
            .into_iter()
            .map(|(_, member)| member)
            .collect::<Vec<_>>();
        let payload = json!({
            "type": "task_discovery_results",
            "task_id": self.task.task_id.as_str(),
            "planner_attempt_id": self.planner_attempt_id.as_str(),
            "message": "All planner discovery probes are terminal. Use these bounded results now and continue directly to task_plan_update. Do not call request_task_discovery, spawn_agent, spawn_agents, or wait_agent.",
            "members": members,
        });
        Ok(ToolResult::ok(
            call.id.clone(),
            call.name.clone(),
            payload.to_string(),
            ToolResultMeta {
                details: payload,
                ..ToolResultMeta::default()
            },
        ))
    }
}

#[async_trait]
impl sigil_kernel::AgentToolDelegate for TaskDiscoveryDelegate<'_> {
    fn set_run_cancellation(&mut self, cancellation: Option<RunCancellationHandle>) {
        self.cancellation = cancellation;
    }

    fn set_web_task_tree_budget(&mut self, budget: Option<Arc<WebTaskTreeBudget>>) {
        self.web_task_tree_budget = budget;
    }

    async fn handle_agent_tool_call(
        &mut self,
        _planner_session: &mut Session,
        call: &ToolCall,
        _options: &AgentRunOptions,
        handler: &mut (dyn EventHandler + Send),
        _approval_handler: &mut (dyn ApprovalHandler + Send),
    ) -> Result<Option<ToolResult>> {
        if call.name != REQUEST_TASK_DISCOVERY_TOOL_NAME {
            return Ok(None);
        }
        let args = match serde_json::from_str::<Value>(&call.args_json) {
            Ok(args) => args,
            Err(error) => {
                return Ok(Some(discovery_error(
                    call,
                    ToolErrorKind::InvalidInput,
                    &format!("invalid planner discovery arguments: {error}"),
                )));
            }
        };
        self.run_discovery(call, &args, handler).await.map(Some)
    }
}

struct TaskDiscoveryTool {
    max_probes: usize,
}

#[async_trait]
impl Tool for TaskDiscoveryTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: REQUEST_TASK_DISCOVERY_TOOL_NAME.to_owned(),
            description: "Run one host-owned batch of independent read-only Explore probes before producing the task plan. Call at most once in this planning attempt. The host waits for all terminal results and resumes this planner automatically; never call wait_agent.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "probes": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": self.max_probes,
                        "items": {
                            "type": "object",
                            "properties": {
                                "probe_id": {
                                    "type": "string",
                                    "description": "Stable unique id for this probe in the planning attempt."
                                },
                                "title": { "type": "string" },
                                "objective": { "type": "string" },
                                "path_hints": {
                                    "type": "array",
                                    "maxItems": MAX_DISCOVERY_PATH_HINTS,
                                    "items": { "type": "string" }
                                }
                            },
                            "required": ["probe_id", "title", "objective"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["probes"],
                "additionalProperties": false
            }),
            category: ToolCategory::Agent,
            access: ToolAccess::Execute,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_subjects(&self, _ctx: &ToolContext, _args: &Value) -> Result<Vec<ToolSubject>> {
        Ok(vec![ToolSubject::agent(EXPLORE_PROFILE_ID.to_owned())])
    }

    fn permission_default_mode(
        &self,
        _ctx: &ToolContext,
        _args: &Value,
    ) -> Result<Option<ApprovalMode>> {
        Ok(Some(ApprovalMode::Allow))
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: Value,
    ) -> Result<ToolResult> {
        Ok(ToolResult::error(
            call_id,
            REQUEST_TASK_DISCOVERY_TOOL_NAME,
            ToolErrorKind::Unsupported,
            "request_task_discovery requires the isolated planner runtime delegate",
        ))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTaskDiscoveryArgs {
    probes: Vec<RawTaskDiscoveryProbe>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTaskDiscoveryProbe {
    probe_id: String,
    title: String,
    objective: String,
    #[serde(default)]
    path_hints: Vec<String>,
}

#[derive(Clone)]
struct TaskDiscoveryProbe {
    probe_id: AgentRouteId,
    title: String,
    objective: String,
    path_hints: Vec<String>,
}

struct PreparedDiscoveryProbe {
    sequence: u64,
    probe: TaskDiscoveryProbe,
    start: AgentTaskChildStart,
    child_session: Session,
    child_session_ref: SessionRef,
    child_input: AgentRunInput,
}

struct StartedDiscoveryProbe {
    sequence: u64,
    probe: TaskDiscoveryProbe,
    thread: AgentTaskChildThread,
    child_session: Session,
    child_session_ref: SessionRef,
    child_input: AgentRunInput,
}

struct DiscoveryCompletionContext {
    probe: TaskDiscoveryProbe,
    thread: AgentTaskChildThread,
    _release_guard: DiscoveryThreadReleaseGuard,
}

struct DiscoveryProbeOutput {
    child_session_ref: SessionRef,
    materialized: AgentResultMaterialization,
    outcome: sigil_kernel::AgentRunOutcome,
    usage: sigil_kernel::AgentUsageSummary,
    status: TaskChildSessionStatus,
}

struct DiscoveryThreadReleaseGuard {
    supervisor: AgentSupervisor,
    thread_id: AgentThreadId,
}

impl DiscoveryThreadReleaseGuard {
    fn new(supervisor: &AgentSupervisor, thread: &AgentTaskChildThread) -> Self {
        Self {
            supervisor: supervisor.clone(),
            thread_id: thread.thread_id.clone(),
        }
    }
}

impl Drop for DiscoveryThreadReleaseGuard {
    fn drop(&mut self) {
        self.supervisor.release_runtime_thread(&self.thread_id);
    }
}

async fn run_discovery_probe(
    agent: Arc<BoxedAgent>,
    thread: AgentTaskChildThread,
    mut child_session: Session,
    child_session_ref: SessionRef,
    child_input: AgentRunInput,
    options: AgentRunOptions,
) -> Result<DiscoveryProbeOutput> {
    let mut handler = DiscoveryEventHandler;
    let mut approval = DiscoveryApprovalHandler;
    let output = agent
        .run_with_approval_input(
            &mut child_session,
            child_input,
            options,
            &mut handler,
            &mut approval,
        )
        .await?;
    let status = task_child_status_from_outcome(&output.result.final_text, &output.outcome);
    let materialized = materialize_child_agent_final_answer(
        &mut child_session,
        &child_session_ref,
        &thread.thread_id,
        &output.result,
    )
    .await?;
    Ok(DiscoveryProbeOutput {
        child_session_ref,
        materialized,
        outcome: output.outcome,
        usage: usage_summary_from_stats(child_session.stats()),
        status,
    })
}

struct DiscoveryEventHandler;

impl EventHandler for DiscoveryEventHandler {
    fn handle(&mut self, _event: RunEvent) -> Result<()> {
        Ok(())
    }
}

struct DiscoveryApprovalHandler;

impl ApprovalHandler for DiscoveryApprovalHandler {
    fn approve_tool_call(&mut self, _call: &ToolCall, _spec: &ToolSpec) -> Result<ToolApproval> {
        Ok(ToolApproval::Deny {
            reason: "planner discovery is non-interactive and cannot widen read permissions"
                .to_owned(),
        })
    }
}

fn parse_task_discovery_probes(args: &Value, max_probes: usize) -> Result<Vec<TaskDiscoveryProbe>> {
    let raw: RawTaskDiscoveryArgs =
        serde_json::from_value(args.clone()).context("invalid planner discovery arguments")?;
    if raw.probes.is_empty() {
        bail!("planner discovery requires at least one probe");
    }
    if raw.probes.len() > max_probes.min(MAX_TASK_DISCOVERY_PROBES) {
        bail!(
            "planner discovery requested {} probes, maximum is {}",
            raw.probes.len(),
            max_probes.min(MAX_TASK_DISCOVERY_PROBES)
        );
    }
    let mut ids = BTreeSet::new();
    let mut objectives = BTreeSet::new();
    let mut probes = Vec::with_capacity(raw.probes.len());
    for raw_probe in raw.probes {
        let probe_id = AgentRouteId::new(raw_probe.probe_id)?;
        if !ids.insert(probe_id.clone()) {
            bail!(
                "planner discovery contains duplicate probe_id {}",
                probe_id.as_str()
            );
        }
        let title = bounded_required_text(
            "planner discovery title",
            &raw_probe.title,
            MAX_DISCOVERY_TITLE_CHARS,
        )?;
        let objective = bounded_required_text(
            "planner discovery objective",
            &raw_probe.objective,
            MAX_DISCOVERY_OBJECTIVE_CHARS,
        )?;
        if !objectives.insert(objective.clone()) {
            bail!("planner discovery contains duplicate objective");
        }
        if raw_probe.path_hints.len() > MAX_DISCOVERY_PATH_HINTS {
            bail!(
                "planner discovery probe {} has too many path hints",
                probe_id.as_str()
            );
        }
        let mut path_hints = BTreeSet::new();
        for hint in raw_probe.path_hints {
            path_hints.insert(normalize_discovery_path_hint(&hint)?);
        }
        probes.push(TaskDiscoveryProbe {
            probe_id,
            title,
            objective,
            path_hints: path_hints.into_iter().collect(),
        });
    }
    reject_overlapping_probe_paths(&probes)?;
    probes.sort_by(|left, right| left.probe_id.cmp(&right.probe_id));
    Ok(probes)
}

fn bounded_required_text(label: &str, value: &str, max_chars: usize) -> Result<String> {
    let value = sigil_kernel::safe_persistence_text(value).trim().to_owned();
    if value.is_empty() {
        bail!("{label} cannot be empty");
    }
    if value.chars().count() > max_chars {
        bail!("{label} exceeds {max_chars} characters");
    }
    Ok(value)
}

fn normalize_discovery_path_hint(value: &str) -> Result<String> {
    let value = sigil_kernel::safe_persistence_text(value).trim().to_owned();
    if value.is_empty() {
        bail!("planner discovery path hint cannot be empty");
    }
    if value.chars().count() > MAX_DISCOVERY_PATH_CHARS {
        bail!("planner discovery path hint exceeds {MAX_DISCOVERY_PATH_CHARS} characters");
    }
    let path = Path::new(&value);
    if path.is_absolute() {
        bail!("planner discovery path hint must be workspace-relative");
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(segment) => normalized.push(segment),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("planner discovery path hint cannot escape the workspace")
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        bail!("planner discovery path hint cannot resolve to the workspace root");
    }
    Ok(normalized.to_string_lossy().into_owned())
}

fn reject_overlapping_probe_paths(probes: &[TaskDiscoveryProbe]) -> Result<()> {
    for (index, left) in probes.iter().enumerate() {
        for right in probes.iter().skip(index + 1) {
            for left_path in &left.path_hints {
                for right_path in &right.path_hints {
                    let left_path = Path::new(left_path);
                    let right_path = Path::new(right_path);
                    if left_path == right_path
                        || left_path.starts_with(right_path)
                        || right_path.starts_with(left_path)
                    {
                        bail!(
                            "planner discovery probes {} and {} have overlapping path hints",
                            left.probe_id.as_str(),
                            right.probe_id.as_str()
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

fn discovery_child_task_id(
    task_id: &TaskId,
    planner_attempt_id: &TaskParticipantAttemptId,
    probe_id: &AgentRouteId,
) -> Result<TaskId> {
    TaskId::new(format!(
        "discovery-{}",
        short_digest(&hash_text(&format!(
            "{}:{}:{}",
            task_id.as_str(),
            planner_attempt_id.as_str(),
            probe_id.as_str()
        )))
    ))
}

fn discovery_logical_run_id(
    planner_attempt_id: &TaskParticipantAttemptId,
    probe_id: &AgentRouteId,
) -> String {
    format!(
        "task-discovery:{}:{}",
        planner_attempt_id.as_str(),
        probe_id.as_str()
    )
}

fn discovery_probe_prompt(task_objective: &str, probe: &TaskDiscoveryProbe) -> String {
    let path_hints = if probe.path_hints.is_empty() {
        "-".to_owned()
    } else {
        probe.path_hints.join("\n- ")
    };
    format!(
        "Task objective:\n{task_objective}\n\nProbe title:\n{}\n\nAssigned objective:\n{}\n\nWorkspace-relative path hints:\n- {path_hints}",
        probe.title, probe.objective
    )
}

fn discovery_member_from_projection(
    parent_session: &Session,
    probe: &TaskDiscoveryProbe,
    thread: &AgentTaskChildThread,
) -> Value {
    let projection = parent_session.agent_thread_state_projection();
    let projected = projection.threads.get(&thread.thread_id);
    let result = projected.and_then(|thread| thread.result.as_ref());
    json!({
        "probe_id": probe.probe_id.as_str(),
        "title": probe.title,
        "status": result.map_or("unavailable", |result| match result.status {
            sigil_kernel::AgentThreadTerminalStatus::Completed => "completed",
            sigil_kernel::AgentThreadTerminalStatus::Failed => "failed",
            sigil_kernel::AgentThreadTerminalStatus::Cancelled => "cancelled",
            sigil_kernel::AgentThreadTerminalStatus::Interrupted => "interrupted",
            sigil_kernel::AgentThreadTerminalStatus::Unknown => "unknown",
        }),
        "summary": result.map(|result| result.summary.as_str()),
        "summary_truncated": result.is_some_and(|result| result.summary_truncated),
        "result_ref": result.map(|result| json!({
            "thread_id": result.thread_id.as_str(),
            "session_ref": result.session_ref.as_path().display().to_string(),
            "output_hash": result.output_hash,
            "final_answer_ref": result.final_answer_ref.as_ref().map(|reference| json!({
                "session_ref": reference.session_ref.as_path().display().to_string(),
                "message_id": reference.message_id,
                "content_hash": reference.content_hash,
                "char_count": reference.char_count,
            })),
        })),
        "changed_paths": result.map(|result| &result.changed_paths),
    })
}

fn discovery_member_error(
    probe: &TaskDiscoveryProbe,
    thread: &AgentTaskChildThread,
    reason: &str,
) -> Value {
    json!({
        "probe_id": probe.probe_id.as_str(),
        "title": probe.title,
        "thread_id": thread.thread_id.as_str(),
        "status": "failed",
        "summary": sigil_kernel::safe_persistence_text(reason)
            .chars()
            .take(1_000)
            .collect::<String>(),
        "summary_truncated": reason.chars().count() > 1_000,
        "result_ref": null,
        "changed_paths": [],
    })
}

fn discovery_error(call: &ToolCall, kind: ToolErrorKind, message: &str) -> ToolResult {
    ToolResult::error(call.id.clone(), call.name.clone(), kind, message.to_owned())
        .with_error_details(
            false,
            json!({
                "error": "task_discovery_rejected",
                "message": message,
                "whole_batch_rejected": true,
                "provider_started": false,
            }),
        )
}
