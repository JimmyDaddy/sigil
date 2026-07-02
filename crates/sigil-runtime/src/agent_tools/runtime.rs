use super::*;

/// Runtime delegate that executes approved agent-thread tool calls.
pub struct AgentToolRuntime {
    pub(super) supervisor: AgentSupervisor,
    pub(super) root_config: RootConfig,
    pub(super) base_registry: ToolRegistry,
    pub(super) provider_factory: Arc<dyn AgentToolProviderFactory>,
    pub(super) background_runs: AgentToolBackgroundRuns,
    pub(super) pending_waits: BTreeMap<AgentThreadId, Instant>,
}

/// Result of a user-directed foreground agent invocation.
pub struct ManualAgentInvocationResult {
    pub thread_id: AgentThreadId,
    pub status: Option<AgentThreadStatus>,
    pub result: Option<AgentThreadResult>,
}

impl AgentToolRuntime {
    #[must_use]
    pub fn new(
        supervisor: AgentSupervisor,
        root_config: RootConfig,
        base_registry: ToolRegistry,
    ) -> Self {
        Self {
            supervisor,
            root_config,
            base_registry,
            provider_factory: Arc::new(DefaultAgentToolProviderFactory),
            background_runs: AgentToolBackgroundRuns::default(),
            pending_waits: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_provider_factory(
        supervisor: AgentSupervisor,
        root_config: RootConfig,
        base_registry: ToolRegistry,
        provider_factory: Arc<dyn AgentToolProviderFactory>,
    ) -> Self {
        Self {
            supervisor,
            root_config,
            base_registry,
            provider_factory,
            background_runs: AgentToolBackgroundRuns::default(),
            pending_waits: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_background_runs(mut self, background_runs: AgentToolBackgroundRuns) -> Self {
        self.background_runs = background_runs;
        self
    }

    pub async fn collect_finished_background_runs(
        &mut self,
        session: &mut Session,
        handler: &mut (dyn EventHandler + Send),
    ) -> Result<Vec<AgentThreadId>> {
        let finished = self.background_runs.take_finished();
        let mut thread_ids = Vec::new();
        for background in finished {
            thread_ids.push(
                self.record_finished_background_run(session, handler, background)
                    .await?,
            );
        }
        Ok(thread_ids)
    }

    pub(super) fn resolve_spawn_profile(
        &self,
        profile_id: &AgentProfileId,
    ) -> Result<ResolvedAgentProfile> {
        let resolved = self
            .supervisor
            .registry()
            .get(profile_id)
            .with_context(|| format!("agent profile {} is not registered", profile_id.as_str()))?;
        if !resolved.effective_enabled() {
            return Err(anyhow!("agent profile {} is disabled", profile_id.as_str()));
        }
        if resolved.trust_state != AgentTrustState::Trusted {
            return Err(anyhow!(
                "agent profile {} is not trusted",
                profile_id.as_str()
            ));
        }
        if !resolved.effective_model_invocation_allowed() {
            return Err(anyhow!(
                "agent profile {} is not model-invocable",
                profile_id.as_str()
            ));
        }
        Ok(resolved.clone())
    }

    fn resolve_manual_profile(&self, profile_id: &AgentProfileId) -> Result<ResolvedAgentProfile> {
        let resolved = self
            .supervisor
            .registry()
            .get(profile_id)
            .with_context(|| format!("agent profile {} is not registered", profile_id.as_str()))?;
        if !resolved.effective_enabled() {
            return Err(anyhow!("agent profile {} is disabled", profile_id.as_str()));
        }
        if resolved.trust_state != AgentTrustState::Trusted {
            return Err(anyhow!(
                "agent profile {} is not trusted",
                profile_id.as_str()
            ));
        }
        if !resolved.effective_user_invocation_allowed() {
            return Err(anyhow!(
                "agent profile {} is not user-invocable",
                profile_id.as_str()
            ));
        }
        Ok(resolved.clone())
    }

    pub async fn invoke_agent_profile(
        &mut self,
        session: &mut Session,
        profile_id: AgentProfileId,
        prompt: String,
        options: &sigil_kernel::AgentRunOptions,
        handler: &mut (dyn EventHandler + Send),
        approval_handler: &mut (dyn ApprovalHandler + Send),
    ) -> Result<ManualAgentInvocationResult> {
        let resolved_profile = self.resolve_manual_profile(&profile_id)?;
        let call_id = manual_agent_call_id(session, &profile_id, &prompt);
        let call = ToolCall {
            id: call_id,
            name: SPAWN_AGENT_TOOL_NAME.to_owned(),
            args_json: json!({
                "profile_id": profile_id.as_str(),
                "objective": &prompt,
                "prompt": &prompt,
                "mode": "join_before_final",
            })
            .to_string(),
        };
        let request = ChatAgentRunRequest {
            profile_id,
            objective: prompt.clone(),
            prompt,
            mode: AgentInvocationMode::JoinBeforeFinal,
            display_name_hint: None,
            invocation_source: AgentInvocationSource::Mention,
            resolved_profile,
        };
        let thread_id = self
            .run_chat_agent(session, &call, request, options, handler, approval_handler)
            .await?;
        let projection = session.agent_thread_state_projection();
        let thread = projection.threads.get(&thread_id);
        let status = thread.map(|thread| thread.status);
        let result = thread.and_then(|thread| thread.result.clone());
        Ok(ManualAgentInvocationResult {
            thread_id,
            status,
            result,
        })
    }

    pub async fn route_agent_message(
        &mut self,
        session: &mut Session,
        thread_id: AgentThreadId,
        prompt: String,
        options: &AgentRunOptions,
    ) -> Result<(ToolResult, Vec<ControlEntry>)> {
        let call = ToolCall {
            id: format!("runtime-message-agent-{}", thread_id.as_str()),
            name: MESSAGE_AGENT_TOOL_NAME.to_owned(),
            args_json: json!({
                "thread_id": thread_id.as_str(),
                "prompt": prompt,
            })
            .to_string(),
        };
        let mut handler = NoopAgentToolEventHandler;
        let mut approval = sigil_kernel::AutoApproveHandler;
        let mut result = self
            .handle_agent_tool_call(session, &call, options, &mut handler, &mut approval)
            .await?
            .ok_or_else(|| anyhow!("message_agent was not handled by runtime"))?;
        let controls = std::mem::take(&mut result.control_entries);
        for control in controls.iter().cloned() {
            session
                .append_control(control)
                .context("failed to append agent message state")?;
        }
        Ok((result, controls))
    }
}

struct NoopAgentToolEventHandler;

impl EventHandler for NoopAgentToolEventHandler {
    fn handle(&mut self, _event: RunEvent) -> Result<()> {
        Ok(())
    }
}

pub trait AgentToolProviderFactory: Send + Sync {
    fn build_provider(
        &self,
        root_config: &RootConfig,
        role: AgentRole,
        profile_id: &AgentProfileId,
    ) -> Result<Box<dyn Provider>>;
}

struct DefaultAgentToolProviderFactory;

impl AgentToolProviderFactory for DefaultAgentToolProviderFactory {
    fn build_provider(
        &self,
        root_config: &RootConfig,
        role: AgentRole,
        _profile_id: &AgentProfileId,
    ) -> Result<Box<dyn Provider>> {
        build_role_provider(root_config, role)
    }
}

#[async_trait]
impl AgentToolDelegate for AgentToolRuntime {
    async fn handle_agent_tool_call(
        &mut self,
        session: &mut Session,
        call: &ToolCall,
        options: &sigil_kernel::AgentRunOptions,
        handler: &mut (dyn EventHandler + Send),
        approval_handler: &mut (dyn ApprovalHandler + Send),
    ) -> Result<Option<ToolResult>> {
        let Some(kind) = AgentToolKind::from_name(&call.name) else {
            return Ok(None);
        };
        self.collect_finished_background_runs(session, handler)
            .await?;
        let args = parse_tool_args(call)?;
        let result = match kind {
            AgentToolKind::Spawn => {
                self.spawn_agent(session, call, &args, options, handler, approval_handler)
                    .await
            }
            AgentToolKind::Wait => self.wait_agent(session, call, &args, handler).await,
            AgentToolKind::ReadResult => self.read_agent_result(session, call, &args),
            AgentToolKind::Message => self.message_agent(session, call, &args),
            AgentToolKind::Close => self.close_agent(session, call, &args),
        };
        Ok(Some(result))
    }

    fn final_answer_blocker(&mut self, session: &mut Session) -> Result<Option<String>> {
        let projection = session.agent_thread_state_projection();
        let pending = projection
            .threads
            .values()
            .filter(|thread| {
                thread.invocation_mode == Some(AgentInvocationMode::JoinBeforeFinal)
                    && !thread.status.is_terminal()
                    && !agent_thread_is_backgrounded(thread)
            })
            .map(|thread| {
                json!({
                    "thread_id": thread.thread_id.as_str(),
                    "display_name": thread.display_name.as_deref(),
                    "status": thread_status_label(thread.status),
                    "objective": &thread.objective,
                    "required_action": {
                        "tool": WAIT_AGENT_TOOL_NAME,
                        "args": { "thread_id": thread.thread_id.as_str() }
                    }
                })
            })
            .collect::<Vec<_>>();
        if !pending.is_empty() {
            return Ok(Some(
                json!({
                    "error": "join_before_final_agent_pending",
                    "message": "A join-before-final child agent is still running. Do not give the final answer yet; wait for the agent result or read the result if it is ready.",
                    "pending_threads": pending,
                    "session_facts": session_facts_summary(session)
                })
                .to_string(),
            ));
        }
        Ok(None)
    }

    fn final_answer_context(&mut self, session: &Session) -> Result<Option<FinalAnswerContext>> {
        let facts = collect_session_facts(session);
        if !facts.has_recorded_facts {
            return Ok(None);
        }
        let key = hash_text(&serde_json::to_string(&facts.value)?);
        let prompt = json!({
            "type": "run_facts_summary",
            "message": "Use these recorded run facts when composing the final answer. Do not claim checks, commands, approvals, subagent results, or file changes that are not listed here. If background agents are still running, say that they are still running rather than implying their work is complete. If a command has exit_code or verdict, do not rerun it only to recover truncated output.",
            "session_facts": facts.value
        })
        .to_string();
        Ok(Some(FinalAnswerContext { key, prompt }))
    }
}

fn agent_thread_is_backgrounded(thread: &sigil_kernel::AgentThreadProjection) -> bool {
    thread.invocation_mode == Some(AgentInvocationMode::Background)
        || thread.reason.as_deref() == Some("agent moved to background")
        || thread.attempts.values().any(|attempt| attempt.background)
}

struct SessionFactsSummary {
    value: Value,
    has_recorded_facts: bool,
}

fn session_facts_summary(session: &Session) -> Value {
    collect_session_facts(session).value
}

fn collect_session_facts(session: &Session) -> SessionFactsSummary {
    let mut approvals_requested = 0_u64;
    let mut approvals_resolved = 0_u64;
    let mut approval_session_grants = 0_u64;
    let mut approval_subject_counts = BTreeMap::<String, u64>::new();
    let mut commands = Vec::new();
    let mut gates = Vec::new();
    let mut changed_files = std::collections::BTreeSet::<String>::new();

    for entry in session.entries() {
        let SessionLogEntry::Control(control) = entry else {
            continue;
        };
        match control {
            ControlEntry::ToolApproval(approval) => {
                if approval.action == ToolApprovalAuditAction::Requested {
                    approvals_requested += 1;
                    let key = approval
                        .subjects
                        .iter()
                        .map(|subject| subject.normalized.as_str())
                        .collect::<Vec<_>>()
                        .join("|");
                    if !key.is_empty() {
                        *approval_subject_counts.entry(key).or_default() += 1;
                    }
                } else if approval.action == ToolApprovalAuditAction::Resolved {
                    approvals_resolved += 1;
                }
            }
            ControlEntry::ToolApprovalSessionGrant(_) => {
                approval_session_grants += 1;
            }
            ControlEntry::ToolExecution(execution) => {
                if execution.status != ToolExecutionStatus::Started {
                    for file in &execution.changed_files {
                        changed_files.insert(file.clone());
                    }
                    let shell = execution.metadata.details.get("shell");
                    let command = shell
                        .and_then(|shell| shell.get("command"))
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    let command_family = shell
                        .and_then(|shell| shell.get("command_family"))
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    let verdict = shell
                        .and_then(|shell| shell.get("verdict"))
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    let command_fact = json!({
                        "tool": execution.tool_name.as_str(),
                        "status": tool_execution_status_label(execution.status),
                        "command": command,
                        "command_family": command_family,
                        "exit_code": execution.metadata.exit_code,
                        "verdict": verdict,
                        "changed_files": &execution.changed_files,
                    });
                    if let Some(family) = command_fact.get("command_family").and_then(Value::as_str)
                        && matches!(
                            family,
                            "cargo_check" | "cargo_fmt_check" | "cargo_test" | "check_touched"
                        )
                    {
                        gates.push(command_fact.clone());
                    }
                    commands.push(command_fact);
                }
            }
            _ => {}
        }
    }

    let repeated_approvals = approval_subject_counts
        .values()
        .map(|count| count.saturating_sub(1))
        .sum::<u64>();
    let projection = session.agent_thread_state_projection();
    let mut subagents_total = 0_u64;
    let mut subagents_running = 0_u64;
    let mut subagents_terminal = 0_u64;
    let mut subagent_threads = Vec::new();
    for thread in projection.threads.values() {
        if thread.thread_id.as_str() == MAIN_THREAD_ID {
            continue;
        }
        subagents_total += 1;
        if thread.status.is_terminal() {
            subagents_terminal += 1;
        } else {
            subagents_running += 1;
        }
        subagent_threads.push(json!({
            "thread_id": thread.thread_id.as_str(),
            "display_name": thread.display_name.as_deref(),
            "status": thread_status_label(thread.status),
            "objective": &thread.objective,
            "mode": thread.invocation_mode.map(invocation_mode_label),
        }));
    }

    let has_recorded_facts = approvals_requested > 0
        || !commands.is_empty()
        || subagents_total > 0
        || !changed_files.is_empty();
    SessionFactsSummary {
        has_recorded_facts,
        value: json!({
            "approvals": {
                "requested": approvals_requested,
                "resolved": approvals_resolved,
                "session_grants": approval_session_grants,
                "repeated_approval_count": repeated_approvals,
            },
            "commands": commands,
            "gates": gates,
            "subagents": {
                "total": subagents_total,
                "running": subagents_running,
                "terminal": subagents_terminal,
                "threads": subagent_threads,
            },
            "files_changed": changed_files.into_iter().collect::<Vec<_>>(),
        }),
    }
}

fn tool_execution_status_label(status: ToolExecutionStatus) -> &'static str {
    match status {
        ToolExecutionStatus::Started => "started",
        ToolExecutionStatus::Completed => "completed",
        ToolExecutionStatus::Failed => "failed",
        ToolExecutionStatus::Cancelled => "cancelled",
        ToolExecutionStatus::Interrupted => "interrupted",
    }
}
