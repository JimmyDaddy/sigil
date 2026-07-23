use super::*;

/// The actual child-thread execution is handled by [`AgentToolRuntime`]. These tool
/// implementations provide stable schemas, permission subjects, previews, and a safe fallback
/// error if an entrypoint registers them without a delegation hook.
pub fn register_agent_tools(registry: &mut ToolRegistry, root_config: &RootConfig) -> Result<()> {
    let profile_registry = AgentProfileRegistry::from_root_config(root_config)?;
    let budget = AgentBudgetPolicy::from_root_config(root_config);
    register_agent_tools_with_registry_and_mode(
        registry,
        profile_registry,
        budget,
        root_config.task.multi_agent_mode,
    )
}

pub fn register_agent_tools_with_workspace(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    workspace_root: &Path,
) -> Result<()> {
    let profile_registry =
        AgentProfileRegistry::from_root_config_with_workspace(root_config, workspace_root)?;
    let budget = AgentBudgetPolicy::from_root_config(root_config);
    register_agent_tools_with_registry_and_mode(
        registry,
        profile_registry,
        budget,
        root_config.task.multi_agent_mode,
    )
}

pub fn register_agent_tools_with_workspace_and_entries(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    workspace_root: &Path,
    entries: &[SessionLogEntry],
) -> Result<()> {
    let profile_registry = AgentProfileRegistry::from_root_config_with_workspace_and_entries(
        root_config,
        workspace_root,
        entries,
    )?;
    let budget = AgentBudgetPolicy::from_root_config(root_config);
    register_agent_tools_with_registry_and_mode(
        registry,
        profile_registry,
        budget,
        root_config.task.multi_agent_mode,
    )
}

pub fn register_agent_tools_with_registry(
    registry: &mut ToolRegistry,
    profile_registry: AgentProfileRegistry,
    budget: AgentBudgetPolicy,
) -> Result<()> {
    register_agent_tools_with_registry_and_mode(
        registry,
        profile_registry,
        budget,
        MultiAgentMode::default(),
    )
}

pub fn register_agent_tools_with_registry_and_mode(
    registry: &mut ToolRegistry,
    profile_registry: AgentProfileRegistry,
    budget: AgentBudgetPolicy,
    multi_agent_mode: MultiAgentMode,
) -> Result<()> {
    let index = profile_registry.model_visible_index(&Default::default())?;
    let base_tool_contracts = registry.contracts();
    let surface = Arc::new(AgentToolSurface {
        profile_registry,
        budget,
        multi_agent_mode,
        base_tool_contracts,
        profile_index_description: profile_index_description(&index),
    });
    for kind in AgentToolKind::ALL {
        registry.register(Arc::new(AgentTool {
            kind,
            surface: Arc::clone(&surface),
        }));
    }
    Ok(())
}

/// Builds the same close result used by the model-visible `close_agent` tool.
#[must_use]
pub fn close_agent_thread(
    session: &Session,
    thread_id: AgentThreadId,
    reason: Option<String>,
) -> ToolResult {
    let thread_id_value = thread_id.as_str().to_owned();
    let args = match reason {
        Some(reason) => json!({
            "thread_id": thread_id_value,
            "reason": reason,
        }),
        None => json!({
            "thread_id": thread_id_value,
        }),
    };
    let call = ToolCall {
        id: format!("runtime-close-agent-{}", thread_id.as_str()),
        name: CLOSE_AGENT_TOOL_NAME.to_owned(),
        args_json: args.to_string(),
    };
    close_agent_from_args(session, &call, &args)
}

struct AgentToolSurface {
    profile_registry: AgentProfileRegistry,
    budget: AgentBudgetPolicy,
    multi_agent_mode: MultiAgentMode,
    base_tool_contracts: Vec<(ToolSpec, sigil_kernel::ToolMutationTracking)>,
    profile_index_description: String,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum AgentToolKind {
    Spawn,
    SpawnBatch,
    Wait,
    ReadResult,
    List,
    Cancel,
    Message,
    Close,
}

impl AgentToolKind {
    const ALL: [Self; 8] = [
        Self::Spawn,
        Self::SpawnBatch,
        Self::Wait,
        Self::ReadResult,
        Self::List,
        Self::Cancel,
        Self::Message,
        Self::Close,
    ];

    pub(super) fn from_name(name: &str) -> Option<Self> {
        match name {
            SPAWN_AGENT_TOOL_NAME => Some(Self::Spawn),
            SPAWN_AGENTS_TOOL_NAME => Some(Self::SpawnBatch),
            WAIT_AGENT_TOOL_NAME => Some(Self::Wait),
            READ_AGENT_RESULT_TOOL_NAME => Some(Self::ReadResult),
            LIST_AGENTS_TOOL_NAME => Some(Self::List),
            CANCEL_AGENT_TOOL_NAME => Some(Self::Cancel),
            MESSAGE_AGENT_TOOL_NAME => Some(Self::Message),
            CLOSE_AGENT_TOOL_NAME => Some(Self::Close),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Spawn => SPAWN_AGENT_TOOL_NAME,
            Self::SpawnBatch => SPAWN_AGENTS_TOOL_NAME,
            Self::Wait => WAIT_AGENT_TOOL_NAME,
            Self::ReadResult => READ_AGENT_RESULT_TOOL_NAME,
            Self::List => LIST_AGENTS_TOOL_NAME,
            Self::Cancel => CANCEL_AGENT_TOOL_NAME,
            Self::Message => MESSAGE_AGENT_TOOL_NAME,
            Self::Close => CLOSE_AGENT_TOOL_NAME,
        }
    }
}

struct AgentTool {
    kind: AgentToolKind,
    surface: Arc<AgentToolSurface>,
}

#[async_trait]
impl Tool for AgentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.kind.name().to_owned(),
            description: self.description(),
            input_schema: self.input_schema(),
            category: ToolCategory::Agent,
            access: match self.kind {
                AgentToolKind::Wait | AgentToolKind::ReadResult | AgentToolKind::List => {
                    ToolAccess::Read
                }
                AgentToolKind::Spawn
                | AgentToolKind::SpawnBatch
                | AgentToolKind::Cancel
                | AgentToolKind::Message
                | AgentToolKind::Close => ToolAccess::Execute,
            },
            network_effect: None,
            preview: match self.kind {
                AgentToolKind::Spawn | AgentToolKind::SpawnBatch => ToolPreviewCapability::Required,
                AgentToolKind::Wait
                | AgentToolKind::ReadResult
                | AgentToolKind::List
                | AgentToolKind::Cancel
                | AgentToolKind::Message
                | AgentToolKind::Close => ToolPreviewCapability::Optional,
            },
        }
    }

    fn permission_subjects(&self, _ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        let subject = match self.kind {
            AgentToolKind::Spawn => ToolSubject::agent(required_string(args, "profile_id")?),
            AgentToolKind::SpawnBatch => {
                return Ok(SpawnAgentsArgs::parse(args)?
                    .members
                    .into_iter()
                    .map(|member| ToolSubject::agent(member.spawn.profile_id.as_str().to_owned()))
                    .collect());
            }
            AgentToolKind::List => ToolSubject::agent("all".to_owned()),
            AgentToolKind::Wait
            | AgentToolKind::ReadResult
            | AgentToolKind::Cancel
            | AgentToolKind::Message
            | AgentToolKind::Close => ToolSubject::agent(required_string(args, "thread_id")?),
        };
        Ok(vec![subject])
    }

    fn permission_default_mode(
        &self,
        _ctx: &ToolContext,
        args: &Value,
    ) -> Result<Option<sigil_kernel::ApprovalMode>> {
        Ok(match self.kind {
            AgentToolKind::Spawn | AgentToolKind::SpawnBatch
                if self.surface.multi_agent_mode == MultiAgentMode::None =>
            {
                Some(sigil_kernel::ApprovalMode::Deny)
            }
            AgentToolKind::Spawn if self.safe_model_spawn(args)? => {
                Some(sigil_kernel::ApprovalMode::Allow)
            }
            AgentToolKind::SpawnBatch if self.safe_model_batch_spawn(args)? => {
                Some(sigil_kernel::ApprovalMode::Allow)
            }
            AgentToolKind::Spawn | AgentToolKind::SpawnBatch => {
                Some(sigil_kernel::ApprovalMode::Ask)
            }
            AgentToolKind::Wait | AgentToolKind::ReadResult | AgentToolKind::List => {
                Some(sigil_kernel::ApprovalMode::Allow)
            }
            AgentToolKind::Cancel | AgentToolKind::Message | AgentToolKind::Close => {
                Some(sigil_kernel::ApprovalMode::Allow)
            }
        })
    }
    async fn preview(&self, _ctx: ToolContext, args: Value) -> Result<Option<ToolPreview>> {
        Ok(match self.kind {
            AgentToolKind::Spawn => Some(self.spawn_preview(&args)?),
            AgentToolKind::SpawnBatch => Some(self.spawn_batch_preview(&args)?),
            AgentToolKind::Wait => Some(simple_agent_preview(
                "Wait for agent",
                &format!("thread {}", required_string(&args, "thread_id")?),
            )),
            AgentToolKind::ReadResult => Some(simple_agent_preview(
                "Read agent result",
                &format!("thread {}", required_string(&args, "thread_id")?),
            )),
            AgentToolKind::List => Some(simple_agent_preview("List agents", "all agent threads")),
            AgentToolKind::Cancel => Some(simple_agent_preview(
                "Cancel agent",
                &format!("thread {}", required_string(&args, "thread_id")?),
            )),
            AgentToolKind::Message => Some(simple_agent_preview(
                "Message agent",
                &format!("thread {}", required_string(&args, "thread_id")?),
            )),
            AgentToolKind::Close => Some(simple_agent_preview(
                "Close agent",
                &format!("thread {}", required_string(&args, "thread_id")?),
            )),
        })
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: Value,
    ) -> Result<ToolResult> {
        Ok(ToolResult::error(
            call_id,
            self.kind.name(),
            ToolErrorKind::Unsupported,
            "agent tools require a runtime agent delegation handler",
        ))
    }
}

impl AgentTool {
    fn safe_model_spawn(&self, args: &Value) -> Result<bool> {
        let profile_id = AgentProfileId::new(required_string(args, "profile_id")?)?;
        let Some(resolved) = self.surface.profile_registry.get(&profile_id) else {
            return Ok(false);
        };
        let resolved_contracts = self
            .surface
            .base_tool_contracts
            .iter()
            .filter(|(spec, _)| resolved.profile.tool_scope.allows(&spec.name))
            .cloned()
            .collect::<Vec<_>>();
        Ok(resolved.effective_enabled()
            && resolved.trust_state == AgentTrustState::Trusted
            && resolved.effective_model_invocation_allowed()
            && tool_contracts_are_safe_readonly_for_auto_spawn(&resolved_contracts))
    }

    fn safe_model_batch_spawn(&self, args: &Value) -> Result<bool> {
        for member in &SpawnAgentsArgs::parse(args)?.members {
            if !self.safe_model_spawn(&member.raw_args)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn description(&self) -> String {
        match self.kind {
            AgentToolKind::Spawn => format!(
                "{}\n{}",
                spawn_agent_mode_description(self.surface.multi_agent_mode),
                self.surface.profile_index_description
            ),
            AgentToolKind::SpawnBatch => format!(
                "Spawn 2-4 independent read-only child agents as one host-joined batch. The host preflights the whole batch, reserves every runtime slot atomically, runs accepted members concurrently, and resumes with bounded results without wait_agent polling. Every member must use a trusted model-visible profile whose effective tool contracts are proven read-only. Use this only for non-overlapping delegated scopes.\n{}",
                self.surface.profile_index_description
            ),
            AgentToolKind::Wait => {
                "Join an agent thread and return only when it completes or a long bounded wait interval expires. Runtime and UI own live progress updates; do not repeatedly poll wait_agent while it is running. Does not return child result text; use read_agent_result when the user explicitly needs result details."
                    .to_owned()
            }
            AgentToolKind::ReadResult => {
                "Explicitly read one bounded page from a completed child agent final answer. Use only when the parent needs details beyond the bounded agent summary; do not request full child transcripts. Prefer read_args returned by wait_agent/result_ref and next_read_args returned by this tool. max_chars is a per-page limit; values outside the supported range are clamped and reported in request metadata."
                    .to_owned()
            }
            AgentToolKind::List => {
                "List current agent threads with status, objective, result read args, and whether each thread can be messaged, cancelled, or closed. Use this instead of repeatedly probing individual thread ids."
                    .to_owned()
            }
            AgentToolKind::Cancel => {
                "Cancel a running background child agent owned by this runtime. This aborts the live runtime handle and appends cancelled agent state; close_agent is only for terminal threads."
                    .to_owned()
            }
            AgentToolKind::Message => {
                "Queue follow-up instructions into an active background child agent mailbox. The result reports delivered_to_mailbox plus will_apply_after_current_turn; delivery happens at the child agent's next safe point and does not interrupt an in-flight provider stream or tool execution. wait_agent is still required to collect terminal results."
                    .to_owned()
            }
            AgentToolKind::Close => {
                "Close a completed, failed, cancelled, or interrupted agent thread.".to_owned()
            }
        }
    }

    fn input_schema(&self) -> Value {
        match self.kind {
            AgentToolKind::Spawn => json!({
                "type": "object",
                "properties": {
                    "profile_id": {
                        "type": "string",
                        "description": "Stable agent profile id from the model-visible agent index."
                    },
                    "objective": { "type": "string" },
                    "prompt": { "type": "string" },
                    "mode": {
                        "type": "string",
                        "enum": ["foreground", "join_before_final", "background"],
                        "default": "join_before_final"
                    },
                    "display_name_hint": { "type": "string" }
                },
                "required": ["profile_id", "objective", "prompt"],
                "additionalProperties": false
            }),
            AgentToolKind::SpawnBatch => json!({
                "type": "object",
                "properties": {
                    "members": {
                        "type": "array",
                        "minItems": 2,
                        "maxItems": 4,
                        "items": {
                            "type": "object",
                            "properties": {
                                "request_key": {
                                    "type": "string",
                                    "description": "Stable unique id for this member within the batch."
                                },
                                "profile_id": {
                                    "type": "string",
                                    "description": "Stable agent profile id from the model-visible agent index."
                                },
                                "objective": { "type": "string" },
                                "prompt": { "type": "string" },
                                "display_name_hint": { "type": "string" }
                            },
                            "required": ["request_key", "profile_id", "objective", "prompt"],
                            "additionalProperties": false
                        }
                    },
                    "completion_mode": {
                        "type": "string",
                        "enum": ["join_before_final"],
                        "default": "join_before_final",
                        "description": "This compatibility slice supports host-owned join. Detached background batches remain deferred."
                    }
                },
                "required": ["members"],
                "additionalProperties": false
            }),
            AgentToolKind::Wait => json!({
                "type": "object",
                "properties": {
                    "thread_id": { "type": "string" }
                },
                "required": ["thread_id"],
                "additionalProperties": false
            }),
            AgentToolKind::ReadResult => json!({
                "type": "object",
                "properties": {
                    "thread_id": { "type": "string" },
                    "offset_chars": {
                        "type": "integer",
                        "default": 0,
                        "minimum": 0,
                        "description": "Character offset into the child agent final answer."
                    },
                    "max_chars": {
                        "type": "integer",
                        "default": 40000,
                        "description": "Maximum characters to return from the child agent final answer. Runtime clamps out-of-range values and reports the effective page size."
                    }
                },
                "required": ["thread_id"],
                "additionalProperties": false
            }),
            AgentToolKind::List => json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            AgentToolKind::Cancel => json!({
                "type": "object",
                "properties": {
                    "thread_id": { "type": "string" },
                    "reason": { "type": "string" }
                },
                "required": ["thread_id"],
                "additionalProperties": false
            }),
            AgentToolKind::Message => json!({
                "type": "object",
                "properties": {
                    "thread_id": { "type": "string" },
                    "prompt": { "type": "string" }
                },
                "required": ["thread_id", "prompt"],
                "additionalProperties": false
            }),
            AgentToolKind::Close => json!({
                "type": "object",
                "properties": {
                    "thread_id": { "type": "string" },
                    "reason": { "type": "string" }
                },
                "required": ["thread_id"],
                "additionalProperties": false
            }),
        }
    }

    fn spawn_preview(&self, args: &Value) -> Result<ToolPreview> {
        let parsed = SpawnAgentArgs::parse(args)?;
        let resolved = self
            .surface
            .profile_registry
            .get(&parsed.profile_id)
            .with_context(|| {
                format!(
                    "agent profile {} is not registered",
                    parsed.profile_id.as_str()
                )
            })?;
        let profile = &resolved.profile;
        let body = [
            format!("profile_id: {}", parsed.profile_id.as_str()),
            format!("source: {:?}", resolved.source),
            format!("trust: {:?}", resolved.trust_state),
            format!("mode: {}", invocation_mode_label(parsed.mode)),
            format!("objective: {}", parsed.objective),
            format!(
                "provider: {}",
                profile.provider.as_deref().unwrap_or("session default")
            ),
            format!(
                "model: {}",
                profile.model.as_deref().unwrap_or("session default")
            ),
            format!("tool_scope: {}", tool_scope_summary(&profile.tool_scope)),
            format!("mcp_servers: {}", profile.mcp_servers.len()),
            format!(
                "budget: max_subagents={}",
                self.surface.budget.max_subagents
            ),
        ]
        .join("\n");
        Ok(ToolPreview {
            title: format!("Spawn agent {}", parsed.profile_id.as_str()),
            summary: format!(
                "{} · {} · {}",
                invocation_mode_label(parsed.mode),
                resolved.trust_state_string(),
                resolved.source_string()
            ),
            body,
            changed_files: Vec::new(),
            file_diffs: Vec::new(),
        })
    }

    fn spawn_batch_preview(&self, args: &Value) -> Result<ToolPreview> {
        let parsed = SpawnAgentsArgs::parse(args)?;
        let body = parsed
            .members
            .iter()
            .map(|member| {
                format!(
                    "{}: profile_id={} · objective={}",
                    member.request_key.as_str(),
                    member.spawn.profile_id.as_str(),
                    member.spawn.objective
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolPreview {
            title: format!("Spawn {} agents", parsed.members.len()),
            summary: format!(
                "{} read-only agents · join before final",
                parsed.members.len()
            ),
            body,
            changed_files: Vec::new(),
            file_diffs: Vec::new(),
        })
    }
}

fn spawn_agent_mode_description(mode: MultiAgentMode) -> &'static str {
    match mode {
        MultiAgentMode::None => {
            "Multi-agent delegation is disabled by [task].multi_agent_mode=none. Do not call spawn_agent for ordinary model delegation; use list_agents/wait_agent/read_agent_result/message_agent/cancel_agent/close_agent only to manage already existing agent threads. Use stable profile_id values, not display names."
        }
        MultiAgentMode::ExplicitRequestOnly => {
            "Spawn a child agent only when the user or active AGENTS/skill instructions explicitly ask for delegated, parallel, sub-agent, or child-agent work. A broad request for comprehensive review, deep analysis, or more investigation is not itself delegation authorization. You must delegate the requested non-overlapping scope instead of completing that same scope yourself. Use mode=join_before_final when the final answer or next step depends on the child result; the host joins safe same-batch children before the next model turn, so do not call wait_agent merely to collect them. Use mode=background only when the parent has truly non-overlapping work; after spawning in background, continue only that non-overlapping work and call wait_agent before the final answer. Write-capable worker profiles use foreground changeset-only isolation and cannot run in background until background isolation is available. Use stable profile_id values, not display names."
        }
        MultiAgentMode::Proactive => {
            "Spawn a child agent when the user explicitly asks for delegation, or proactively when parallel non-overlapping work would clearly improve speed or quality. Do not spawn for overlapping work you will also complete yourself. Use mode=join_before_final when the final answer or next step depends on the child result; the host joins safe same-batch children before the next model turn, so do not call wait_agent merely to collect them. Use mode=background only when the parent has truly non-overlapping work; after spawning in background, continue only that non-overlapping work and call wait_agent before the final answer. Write-capable worker profiles use foreground changeset-only isolation and cannot run in background until background isolation is available. Use stable profile_id values, not display names."
        }
    }
}

trait AgentToolResolvedProfileExt {
    fn trust_state_string(&self) -> &'static str;
    fn source_string(&self) -> &'static str;
}

impl AgentToolResolvedProfileExt for crate::ResolvedAgentProfile {
    fn trust_state_string(&self) -> &'static str {
        match self.trust_state {
            sigil_kernel::AgentTrustState::Trusted => "trusted",
            sigil_kernel::AgentTrustState::NeedsReview => "needs_review",
            sigil_kernel::AgentTrustState::Disabled => "disabled",
            sigil_kernel::AgentTrustState::Unknown => "unknown",
        }
    }

    fn source_string(&self) -> &'static str {
        match self.source {
            sigil_kernel::AgentProfileSource::Workspace => "workspace",
            sigil_kernel::AgentProfileSource::User => "user",
            sigil_kernel::AgentProfileSource::Plugin { .. } => "plugin",
            sigil_kernel::AgentProfileSource::Compatibility { .. } => "compatibility",
            sigil_kernel::AgentProfileSource::System => "system",
            sigil_kernel::AgentProfileSource::Unknown => "unknown",
        }
    }
}

pub(super) struct SpawnAgentArgs {
    pub(super) profile_id: AgentProfileId,
    pub(super) objective: String,
    pub(super) prompt: String,
    pub(super) mode: AgentInvocationMode,
    pub(super) display_name_hint: Option<String>,
}

pub(super) struct SpawnAgentsArgs {
    pub(super) members: Vec<SpawnAgentsMemberArgs>,
}

pub(super) struct SpawnAgentsMemberArgs {
    pub(super) request_key: AgentRouteId,
    pub(super) spawn: SpawnAgentArgs,
    pub(super) raw_args: Value,
}

pub(super) struct ChatAgentRunRequest {
    pub(super) profile_id: AgentProfileId,
    pub(super) objective: String,
    pub(super) prompt: String,
    pub(super) mode: AgentInvocationMode,
    pub(super) display_name_hint: Option<String>,
    pub(super) invocation_source: AgentInvocationSource,
    pub(super) resolved_profile: ResolvedAgentProfile,
}

impl SpawnAgentArgs {
    pub(super) fn parse(args: &Value) -> Result<Self> {
        Ok(Self {
            profile_id: AgentProfileId::new(required_string(args, "profile_id")?)?,
            objective: required_string(args, "objective")?,
            prompt: required_string(args, "prompt")?,
            mode: optional_string(args, "mode")
                .as_deref()
                .map(parse_invocation_mode)
                .transpose()?
                .unwrap_or(AgentInvocationMode::JoinBeforeFinal),
            display_name_hint: optional_string(args, "display_name_hint"),
        })
    }
}

impl SpawnAgentsArgs {
    pub(super) fn parse(args: &Value) -> Result<Self> {
        let completion_mode = optional_string(args, "completion_mode")
            .as_deref()
            .map(parse_invocation_mode)
            .transpose()?
            .unwrap_or(AgentInvocationMode::JoinBeforeFinal);
        if completion_mode != AgentInvocationMode::JoinBeforeFinal {
            bail!("spawn_agents currently supports only completion_mode=join_before_final");
        }
        let members = args
            .get("members")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("members must be an array"))?;
        if !(2..=4).contains(&members.len()) {
            bail!("spawn_agents requires between 2 and 4 members");
        }
        let mut request_keys = BTreeSet::new();
        let mut parsed = Vec::with_capacity(members.len());
        for member in members {
            let request_key = AgentRouteId::new(required_string(member, "request_key")?)?;
            if !request_keys.insert(request_key.clone()) {
                bail!(
                    "spawn_agents contains duplicate request_key {}",
                    request_key.as_str()
                );
            }
            let spawn = SpawnAgentArgs {
                profile_id: AgentProfileId::new(required_string(member, "profile_id")?)?,
                objective: required_string(member, "objective")?,
                prompt: required_string(member, "prompt")?,
                mode: AgentInvocationMode::JoinBeforeFinal,
                display_name_hint: optional_string(member, "display_name_hint"),
            };
            parsed.push(SpawnAgentsMemberArgs {
                request_key,
                spawn,
                raw_args: member.clone(),
            });
        }
        parsed.sort_by(|left, right| left.request_key.cmp(&right.request_key));
        Ok(Self { members: parsed })
    }
}
