use super::*;
use sigil_kernel::NetworkEffect;

/// Runtime delegate that executes approved agent-thread tool calls.
pub struct AgentToolRuntime {
    pub(super) supervisor: AgentSupervisor,
    pub(super) root_config: RootConfig,
    pub(super) base_registry: ToolRegistry,
    pub(super) provider_factory: Arc<dyn AgentToolProviderFactory>,
    pub(super) background_runs: AgentToolBackgroundRuns,
    pub(super) join_dependencies: Vec<JoinedChatAgentHandle>,
    pub(super) pending_join_contexts: BTreeMap<String, Vec<AgentThreadId>>,
    pub(super) next_join_sequence: u64,
    pub(super) join_batch_eligible: bool,
    pub(super) pending_waits: BTreeMap<AgentThreadId, Instant>,
    pub(super) run_cancellation: Option<sigil_kernel::RunCancellationHandle>,
    pub(super) web_task_tree_budget: Option<Arc<sigil_kernel::WebTaskTreeBudget>>,
    #[cfg(test)]
    pub(super) delegation_authority_override: Option<DelegationAuthority>,
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
            join_dependencies: Vec::new(),
            pending_join_contexts: BTreeMap::new(),
            next_join_sequence: 0,
            join_batch_eligible: false,
            pending_waits: BTreeMap::new(),
            run_cancellation: None,
            web_task_tree_budget: None,
            #[cfg(test)]
            delegation_authority_override: None,
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
            join_dependencies: Vec::new(),
            pending_join_contexts: BTreeMap::new(),
            next_join_sequence: 0,
            join_batch_eligible: false,
            pending_waits: BTreeMap::new(),
            run_cancellation: None,
            web_task_tree_budget: None,
            #[cfg(test)]
            delegation_authority_override: None,
        }
    }

    #[must_use]
    pub fn with_background_runs(mut self, background_runs: AgentToolBackgroundRuns) -> Self {
        self.background_runs = background_runs;
        self
    }

    /// Test-only injection for exercising host-owned delegation admission.
    ///
    /// Production model-tool runtimes always begin with proactive authority. O2 will expose a
    /// scoped, consumable host grant rather than an ambient authority setter.
    #[cfg(test)]
    #[must_use]
    pub(super) fn with_delegation_authority(mut self, authority: DelegationAuthority) -> Self {
        self.delegation_authority_override = Some(authority);
        self
    }

    pub(super) fn model_delegation_authority(&self) -> DelegationAuthority {
        #[cfg(test)]
        if let Some(authority) = &self.delegation_authority_override {
            return authority.clone();
        }
        DelegationAuthority::ModelProactive
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

    pub(super) fn inherit_web_task_tree_budget(
        &self,
        input: sigil_kernel::AgentRunInput,
    ) -> sigil_kernel::AgentRunInput {
        self.web_task_tree_budget
            .as_ref()
            .map_or(input.clone(), |budget| {
                input.with_web_task_tree_budget(Arc::clone(budget))
            })
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
        let wait_started = Instant::now();
        loop {
            self.collect_finished_background_runs(session, handler)
                .await?;
            let projection = session.agent_thread_state_projection();
            if projection
                .threads
                .get(&thread_id)
                .is_some_and(|thread| thread.status.is_terminal())
            {
                break;
            }
            if saturating_elapsed(wait_started) >= WAIT_AGENT_BACKGROUND_WAIT_TIMEOUT {
                break;
            }
            tokio::time::sleep(WAIT_AGENT_BACKGROUND_POLL_INTERVAL).await;
        }
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

    pub async fn cancel_agent_thread(
        &mut self,
        session: &mut Session,
        thread_id: AgentThreadId,
        reason: Option<String>,
        options: &AgentRunOptions,
    ) -> Result<ToolResult> {
        let call = ToolCall {
            id: format!("runtime-cancel-agent-{}", thread_id.as_str()),
            name: CANCEL_AGENT_TOOL_NAME.to_owned(),
            args_json: json!({
                "thread_id": thread_id.as_str(),
                "reason": reason,
            })
            .to_string(),
        };
        let mut handler = NoopAgentToolEventHandler;
        let mut approval = sigil_kernel::AutoApproveHandler;
        self.handle_agent_tool_call(session, &call, options, &mut handler, &mut approval)
            .await?
            .ok_or_else(|| anyhow!("cancel_agent was not handled by runtime"))
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
    fn set_run_cancellation(&mut self, cancellation: Option<sigil_kernel::RunCancellationHandle>) {
        self.run_cancellation = cancellation;
    }

    fn set_web_task_tree_budget(&mut self, budget: Option<Arc<sigil_kernel::WebTaskTreeBudget>>) {
        self.web_task_tree_budget = budget;
    }

    fn set_join_batch_eligibility(&mut self, calls: &[ToolCall]) {
        self.join_batch_eligible = tool_batch_allows_host_join(calls);
    }

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
            AgentToolKind::SpawnBatch => self.spawn_agents(session, call, &args, options, handler),
            AgentToolKind::Wait => self.wait_agent(session, call, &args, handler).await,
            AgentToolKind::ReadResult => self.read_agent_result(session, call, &args, handler),
            AgentToolKind::List => self.list_agents(session, call),
            AgentToolKind::Cancel => self.cancel_agent(session, call, &args, handler).await,
            AgentToolKind::Message => self.message_agent(session, call, &args),
            AgentToolKind::Close => self.close_agent(session, call, &args),
        };
        Ok(Some(result))
    }

    async fn settle_join_dependencies(
        &mut self,
        session: &mut Session,
        handler: &mut (dyn EventHandler + Send),
    ) -> Result<Option<FinalAnswerContext>> {
        self.settle_current_join_dependencies(session, handler)
            .await
    }

    fn abort_join_dependencies(
        &mut self,
        session: &mut Session,
        handler: &mut (dyn EventHandler + Send),
        reason: &str,
    ) -> Result<()> {
        self.abort_current_join_dependencies(session, handler, reason)
    }

    fn confirm_join_context_delivery(
        &mut self,
        session: &mut Session,
        handler: &mut (dyn EventHandler + Send),
        context_key: &str,
    ) -> Result<()> {
        self.confirm_current_join_context(session, handler, context_key)
    }

    fn cancel_join_context_delivery(
        &mut self,
        session: &mut Session,
        handler: &mut (dyn EventHandler + Send),
        context_keys: &[String],
        reason: &str,
    ) -> Result<()> {
        self.cancel_current_join_contexts(session, handler, context_keys, reason)
    }

    fn final_answer_blocker(&mut self, session: &mut Session) -> Result<Option<String>> {
        let projection = session.agent_thread_state_projection();
        let continuations = session.agent_result_continuation_projection();
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
        let unread_results = projection
            .threads
            .values()
            .filter(|thread| {
                thread.invocation_mode == Some(AgentInvocationMode::JoinBeforeFinal)
                    && thread.status.is_terminal()
                    && thread.result.is_some()
                    && !thread.result_fully_delivered
                    && !agent_thread_is_backgrounded(thread)
                    && continuations.statuses.get(&thread.thread_id)
                        != Some(&AgentResultContinuationStatus::Completed)
            })
            .map(|thread| {
                let offset_chars = thread.result_delivered_chars;
                json!({
                    "thread_id": thread.thread_id.as_str(),
                    "display_name": thread.display_name.as_deref(),
                    "status": thread_status_label(thread.status),
                    "objective": &thread.objective,
                    "result_delivered_chars": thread.result_delivered_chars,
                    "result_fully_delivered": thread.result_fully_delivered,
                    "required_action": {
                        "tool": READ_AGENT_RESULT_TOOL_NAME,
                        "args": {
                            "thread_id": thread.thread_id.as_str(),
                            "offset_chars": offset_chars,
                            "max_chars": MAX_RESULT_PAGE_LIMIT
                        }
                    }
                })
            })
            .collect::<Vec<_>>();
        if !unread_results.is_empty() {
            return Ok(Some(
                json!({
                    "error": "join_before_final_agent_result_unread",
                    "message": "A join-before-final child agent finished, but its result has not been read yet. Do not give the final answer until read_agent_result has delivered the child result.",
                    "unread_threads": unread_results,
                    "session_facts": session_facts_summary(session)
                })
                .to_string(),
            ));
        }
        Ok(None)
    }

    fn final_answer_context(
        &mut self,
        session: &Session,
        options: &AgentRunOptions,
        outcome: &AgentRunOutcome,
    ) -> Result<Option<FinalAnswerContext>> {
        let facts = collect_session_facts(session, Some((options, outcome)))?;
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
    collect_session_facts(session, None)
        .map(|facts| facts.value)
        .unwrap_or_else(|error| {
            json!({
                "error": "session_facts_unavailable",
                "message": error.to_string(),
            })
        })
}

fn collect_session_facts(
    session: &Session,
    run_context: Option<(&AgentRunOptions, &AgentRunOutcome)>,
) -> Result<SessionFactsSummary> {
    let call_belongs_to_current_run = |call_id: &str| match run_context {
        Some((_, outcome)) => outcome
            .tool_call_ids
            .iter()
            .any(|current_call_id| current_call_id == call_id),
        None => true,
    };
    let mut approvals_policy_allow = 0_u64;
    let mut approvals_policy_deny = 0_u64;
    let mut approvals_requested = 0_u64;
    let mut approvals_resolved = 0_u64;
    let mut approvals_user_allow_once = 0_u64;
    let mut approvals_user_allow_session = 0_u64;
    let mut approvals_user_deny = 0_u64;
    let mut approval_session_grants = 0_u64;
    let mut approval_session_grant_reuses = 0_u64;
    let mut local_policy_facets = approval_mode_counts();
    let mut network_policy_facets = approval_mode_counts();
    let mut source_policy_facets = approval_mode_counts();
    let mut network_effects = BTreeMap::from([
        ("none".to_owned(), 0_u64),
        ("read".to_owned(), 0_u64),
        ("mutate".to_owned(), 0_u64),
        ("unknown".to_owned(), 0_u64),
    ]);
    let mut approval_subject_counts = BTreeMap::<String, u64>::new();
    let mut approval_grant_reuses = Vec::new();
    let mut commands = Vec::new();
    let mut gates = Vec::new();
    let mut changed_files = std::collections::BTreeSet::<String>::new();

    for entry in session.entries() {
        let SessionLogEntry::Control(control) = entry else {
            continue;
        };
        match control {
            ControlEntry::ToolApproval(approval) => {
                if !call_belongs_to_current_run(&approval.call_id) {
                    continue;
                }
                record_approval_mode(&mut local_policy_facets, approval.local_policy_decision);
                record_approval_mode(&mut network_policy_facets, approval.network_policy_decision);
                record_approval_mode(&mut source_policy_facets, approval.source_policy_decision);
                let effect = approval
                    .network_effect
                    .map(NetworkEffect::as_str)
                    .unwrap_or("none");
                *network_effects.entry(effect.to_owned()).or_default() += 1;
                match approval.action {
                    ToolApprovalAuditAction::PolicyEvaluated => {
                        if approval.policy_decision == ApprovalMode::Allow {
                            approvals_policy_allow += 1;
                        } else if approval.policy_decision == ApprovalMode::Deny {
                            approvals_policy_deny += 1;
                        }
                        if approval.allow_source == Some(ToolApprovalAllowSource::SessionGrant) {
                            approval_session_grant_reuses += 1;
                            approval_grant_reuses.push(json!({
                                "call_id": approval.call_id.as_str(),
                                "tool_name": approval.tool_name.as_str(),
                                "grant_call_id": approval.grant_call_id.as_deref(),
                                "network_effect": approval.network_effect,
                                "local_policy_decision": approval.local_policy_decision,
                                "network_policy_decision": approval.network_policy_decision,
                                "source_policy_decision": approval.source_policy_decision,
                                "operation": approval.operation,
                                "risk": approval.risk,
                                "subjects": approval
                                    .subjects
                                    .iter()
                                    .map(|subject| subject.normalized.as_str())
                                    .collect::<Vec<_>>(),
                            }));
                        }
                    }
                    ToolApprovalAuditAction::Requested => {
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
                    }
                    ToolApprovalAuditAction::Resolved => {
                        approvals_resolved += 1;
                        match approval.user_decision {
                            Some(ToolApprovalUserDecision::Approved) => {
                                approvals_user_allow_once += 1;
                            }
                            Some(ToolApprovalUserDecision::ApprovedForSession) => {
                                approvals_user_allow_session += 1;
                            }
                            Some(ToolApprovalUserDecision::Denied) => {
                                approvals_user_deny += 1;
                            }
                            None => {}
                        }
                    }
                    ToolApprovalAuditAction::PreviewFailed => {}
                }
            }
            ControlEntry::ToolApprovalSessionGrant(grant)
                if call_belongs_to_current_run(&grant.call_id) =>
            {
                approval_session_grants += 1;
            }
            ControlEntry::ToolExecution(execution) => {
                if !call_belongs_to_current_run(&execution.call_id) {
                    continue;
                }
                if execution.status != ToolExecutionStatus::Started {
                    for file in &execution.changed_files {
                        changed_files.insert(file.clone());
                    }
                    let shell = execution
                        .metadata
                        .details
                        .get("shell_analysis")
                        .or_else(|| execution.metadata.details.get("shell"));
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
                    let rerun_not_needed = execution.metadata.exit_code == Some(0)
                        && verdict.as_deref() == Some("passed");
                    if command.is_some() {
                        let command_fact = json!({
                            "tool": execution.tool_name.as_str(),
                            "status": tool_execution_status_label(execution.status),
                            "command": command,
                            "command_family": command_family,
                            "exit_code": execution.metadata.exit_code,
                            "verdict": verdict,
                            "output_truncated": execution.metadata.truncated,
                            "rerun_not_needed": rerun_not_needed,
                            "changed_files": &execution.changed_files,
                        });
                        if let Some(family) =
                            command_fact.get("command_family").and_then(Value::as_str)
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
            }
            _ => {}
        }
    }
    if let Some((_, outcome)) = run_context {
        for file in &outcome.changed_files {
            changed_files.insert(file.clone());
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
            "result_available": thread.result.is_some(),
            "result_read": thread.result_delivered,
            "result_fully_read": thread.result_fully_delivered,
            "result_delivered_chars": thread.result_delivered_chars,
            "result_delivery_call_ids": &thread.result_delivery_call_ids,
        }));
    }

    let has_recorded_facts = approvals_policy_deny > 0
        || approvals_requested > 0
        || approvals_resolved > 0
        || approval_session_grants > 0
        || approval_session_grant_reuses > 0
        || !commands.is_empty()
        || subagents_total > 0
        || !changed_files.is_empty();
    let readiness = if has_recorded_facts && let Some((options, outcome)) = run_context {
        let entry = sigil_kernel::projected_agent_run_readiness(
            session,
            options,
            "pending_final_answer",
            outcome,
        )?;
        Some(json!({
            "scope": &entry.scope,
            "run_status": entry.evaluation.run_status,
            "verification_verdict": entry.evaluation.verification_verdict,
            "visible_state": entry.evaluation.visible_state,
            "required_actions": &entry.evaluation.required_actions,
            "reasons": &entry.evaluation.reasons,
            "policy_hash": entry.policy_hash,
            "workspace_snapshot_id": entry.workspace_snapshot_id,
        }))
    } else {
        None
    };
    Ok(SessionFactsSummary {
        has_recorded_facts,
        value: json!({
            "approvals": {
                "policy_allow": approvals_policy_allow,
                "policy_deny": approvals_policy_deny,
                "requested": approvals_requested,
                "resolved": approvals_resolved,
                "user_allow_once": approvals_user_allow_once,
                "user_allow_session": approvals_user_allow_session,
                "user_deny": approvals_user_deny,
                "session_grants": approval_session_grants,
                "session_grant_reuses": approval_session_grant_reuses,
                "repeated_approval_count": repeated_approvals,
                "grant_reuses": approval_grant_reuses,
                "facets": {
                    "local_policy": local_policy_facets,
                    "network_policy": network_policy_facets,
                    "source_policy": source_policy_facets,
                    "network_effect": network_effects,
                },
            },
            "commands": commands,
            "gates": gates,
            "readiness": readiness,
            "subagents": {
                "total": subagents_total,
                "running": subagents_running,
                "terminal": subagents_terminal,
                "threads": subagent_threads,
            },
            "files_changed": changed_files.into_iter().collect::<Vec<_>>(),
        }),
    })
}

fn approval_mode_counts() -> BTreeMap<String, u64> {
    BTreeMap::from([
        (ApprovalMode::Allow.as_str().to_owned(), 0),
        (ApprovalMode::Ask.as_str().to_owned(), 0),
        (ApprovalMode::Deny.as_str().to_owned(), 0),
    ])
}

fn record_approval_mode(counts: &mut BTreeMap<String, u64>, mode: ApprovalMode) {
    *counts.entry(mode.as_str().to_owned()).or_default() += 1;
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
