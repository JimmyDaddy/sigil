use super::*;

/// The actual child-thread execution is handled by [`AgentToolRuntime`]. These tool
/// implementations provide stable schemas, permission subjects, previews, and a safe fallback
/// error if an entrypoint registers them without a delegation hook.
pub fn register_agent_tools(registry: &mut ToolRegistry, root_config: &RootConfig) -> Result<()> {
    let profile_registry = AgentProfileRegistry::from_root_config(root_config)?;
    let budget = AgentBudgetPolicy::from_root_config(root_config);
    register_agent_tools_with_registry(registry, profile_registry, budget)
}

pub fn register_agent_tools_with_workspace(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    workspace_root: &Path,
) -> Result<()> {
    let profile_registry =
        AgentProfileRegistry::from_root_config_with_workspace(root_config, workspace_root)?;
    let budget = AgentBudgetPolicy::from_root_config(root_config);
    register_agent_tools_with_registry(registry, profile_registry, budget)
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
    register_agent_tools_with_registry(registry, profile_registry, budget)
}

pub fn register_agent_tools_with_registry(
    registry: &mut ToolRegistry,
    profile_registry: AgentProfileRegistry,
    budget: AgentBudgetPolicy,
) -> Result<()> {
    let index = profile_registry.model_visible_index(&Default::default())?;
    let surface = Arc::new(AgentToolSurface {
        profile_registry,
        budget,
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
    profile_index_description: String,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum AgentToolKind {
    Spawn,
    Wait,
    ReadResult,
    Message,
    Close,
}

impl AgentToolKind {
    const ALL: [Self; 5] = [
        Self::Spawn,
        Self::Wait,
        Self::ReadResult,
        Self::Message,
        Self::Close,
    ];

    pub(super) fn from_name(name: &str) -> Option<Self> {
        match name {
            SPAWN_AGENT_TOOL_NAME => Some(Self::Spawn),
            WAIT_AGENT_TOOL_NAME => Some(Self::Wait),
            READ_AGENT_RESULT_TOOL_NAME => Some(Self::ReadResult),
            MESSAGE_AGENT_TOOL_NAME => Some(Self::Message),
            CLOSE_AGENT_TOOL_NAME => Some(Self::Close),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Spawn => SPAWN_AGENT_TOOL_NAME,
            Self::Wait => WAIT_AGENT_TOOL_NAME,
            Self::ReadResult => READ_AGENT_RESULT_TOOL_NAME,
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
                AgentToolKind::Wait | AgentToolKind::ReadResult => ToolAccess::Read,
                AgentToolKind::Spawn | AgentToolKind::Message | AgentToolKind::Close => {
                    ToolAccess::Execute
                }
            },
            preview: match self.kind {
                AgentToolKind::Spawn => ToolPreviewCapability::Required,
                AgentToolKind::Wait
                | AgentToolKind::ReadResult
                | AgentToolKind::Message
                | AgentToolKind::Close => ToolPreviewCapability::Optional,
            },
        }
    }

    fn permission_subjects(&self, _ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        let subject = match self.kind {
            AgentToolKind::Spawn => ToolSubject::agent(required_string(args, "profile_id")?),
            AgentToolKind::Wait
            | AgentToolKind::ReadResult
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
            AgentToolKind::Spawn if self.safe_model_spawn(args)? => {
                Some(sigil_kernel::ApprovalMode::Allow)
            }
            AgentToolKind::Spawn => Some(sigil_kernel::ApprovalMode::Ask),
            AgentToolKind::Wait | AgentToolKind::ReadResult => {
                Some(sigil_kernel::ApprovalMode::Allow)
            }
            AgentToolKind::Message | AgentToolKind::Close => {
                Some(sigil_kernel::ApprovalMode::Allow)
            }
        })
    }
    async fn preview(&self, _ctx: ToolContext, args: Value) -> Result<Option<ToolPreview>> {
        Ok(match self.kind {
            AgentToolKind::Spawn => Some(self.spawn_preview(&args)?),
            AgentToolKind::Wait => Some(simple_agent_preview(
                "Wait for agent",
                &format!("thread {}", required_string(&args, "thread_id")?),
            )),
            AgentToolKind::ReadResult => Some(simple_agent_preview(
                "Read agent result",
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
        Ok(resolved.effective_enabled()
            && resolved.trust_state == AgentTrustState::Trusted
            && resolved.effective_model_invocation_allowed()
            && tool_scope_is_safe_readonly_for_auto_spawn(&resolved.profile.tool_scope))
    }

    fn description(&self) -> String {
        match self.kind {
            AgentToolKind::Spawn => format!(
                "Spawn a child agent when the user explicitly asks for delegated, parallel, sub-agent, or child-agent work. You must delegate the requested non-overlapping scope instead of completing that same scope yourself. Use mode=join_before_final when the final answer or next step depends on the child result. Use mode=background only when the parent has truly non-overlapping work; after spawning in background, continue only that non-overlapping work and call wait_agent before the final answer. Foreground users may move that run to background before execution. Use stable profile_id values, not display names.\n{}",
                self.surface.profile_index_description
            ),
            AgentToolKind::Wait => {
                "Join an agent thread and return only when it completes or a long bounded wait interval expires. Runtime and UI own live progress updates; do not repeatedly poll wait_agent while it is running. Does not return child result text; use read_agent_result when the user explicitly needs result details."
                    .to_owned()
            }
            AgentToolKind::ReadResult => {
                "Explicitly read a bounded page from a completed child agent final answer. Use only when the parent needs details beyond the bounded agent summary; do not request full child transcripts."
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
                        "minimum": 200,
                        "maximum": 12000,
                        "default": 4000,
                        "description": "Maximum characters to return from the child agent final answer."
                    }
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
                "budget: max_threads={} max_fanout_per_turn={} max_tokens_per_agent={}",
                self.surface.budget.max_threads,
                self.surface.budget.max_spawn_fanout_per_turn,
                self.surface.budget.max_agent_tokens_per_task
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
            sigil_kernel::AgentProfileSource::LegacyTask => "legacy_task",
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
