use super::spawn::profile_uses_changeset_only_write;
use super::*;
use serde::Deserialize;

const MAX_DELEGATION_OBJECTIVE_CHARS: usize = 1_000;
const MAX_DELEGATION_PROMPT_CHARS: usize = 8_000;
const MAX_DELEGATION_DISPLAY_NAME_CHARS: usize = 120;

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
    base_tool_contracts: Vec<sigil_kernel::ToolRuntimeContract>,
    profile_index_description: String,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum AgentToolKind {
    RequestDelegation,
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
    const ALL: [Self; 9] = [
        Self::RequestDelegation,
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
            REQUEST_AGENT_DELEGATION_TOOL_NAME => Some(Self::RequestDelegation),
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
            Self::RequestDelegation => REQUEST_AGENT_DELEGATION_TOOL_NAME,
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
                AgentToolKind::RequestDelegation
                | AgentToolKind::Spawn
                | AgentToolKind::SpawnBatch
                | AgentToolKind::Cancel
                | AgentToolKind::Message
                | AgentToolKind::Close => ToolAccess::Execute,
            },
            network_effect: None,
            preview: match self.kind {
                AgentToolKind::RequestDelegation
                | AgentToolKind::Spawn
                | AgentToolKind::SpawnBatch => ToolPreviewCapability::Required,
                AgentToolKind::Wait
                | AgentToolKind::ReadResult
                | AgentToolKind::List
                | AgentToolKind::Cancel
                | AgentToolKind::Message
                | AgentToolKind::Close => ToolPreviewCapability::Optional,
            },
        }
    }

    fn permission_plan(&self, _ctx: &ToolContext, args: &Value) -> Result<ToolPermissionPlanDraft> {
        let spec = self.spec();
        let (subjects, tool_default_mode, safe_read_only_spawn) = self.permission_inputs(args)?;
        let operation = match self.kind {
            AgentToolKind::RequestDelegation | AgentToolKind::Spawn | AgentToolKind::SpawnBatch => {
                ToolOperation::SpawnAgent
            }
            AgentToolKind::Wait | AgentToolKind::ReadResult | AgentToolKind::List => {
                ToolOperation::Read
            }
            AgentToolKind::Cancel | AgentToolKind::Close => ToolOperation::CloseAgent,
            AgentToolKind::Message => ToolOperation::MessageAgent,
        };
        let mut effects = BTreeSet::new();
        match self.kind {
            AgentToolKind::RequestDelegation => {
                effects.insert(ToolPermissionEffect::AgentLifecycle);
                effects.insert(ToolPermissionEffect::Unknown);
            }
            AgentToolKind::Spawn | AgentToolKind::SpawnBatch => {
                effects.insert(ToolPermissionEffect::AgentLifecycle);
                if safe_read_only_spawn {
                    effects.insert(ToolPermissionEffect::FileRead);
                } else {
                    effects.insert(ToolPermissionEffect::Unknown);
                }
            }
            AgentToolKind::Wait
            | AgentToolKind::ReadResult
            | AgentToolKind::List
            | AgentToolKind::Cancel
            | AgentToolKind::Message
            | AgentToolKind::Close => {
                effects.insert(ToolPermissionEffect::AgentLifecycle);
            }
        }
        let mut semantic_scope =
            ToolSemanticScope::new(format!("agent_thread:{}", self.kind.name()), 1);
        if matches!(self.kind, AgentToolKind::Spawn | AgentToolKind::SpawnBatch) {
            semantic_scope.qualifiers.insert(
                "safe_read_only_profile".to_owned(),
                safe_read_only_spawn.to_string(),
            );
        }
        Ok(ToolPermissionPlanDraft {
            access: spec.access,
            operation,
            effects,
            subjects,
            analysis: sigil_kernel::ToolAnalysisStatus::Complete,
            containment: Default::default(),
            semantic_scope: Some(semantic_scope),
            tool_default_mode,
            managed_file_access: None,
            analysis_bindings: BTreeMap::from([(
                "planner".to_owned(),
                "agent_thread_v2".to_owned(),
            )]),
            safe_summary: ToolPermissionSummary {
                title: self.kind.name().replace('_', " "),
                detail: match self.kind {
                    AgentToolKind::RequestDelegation => {
                        "Request explicit authority for a bounded agent batch".to_owned()
                    }
                    AgentToolKind::Spawn | AgentToolKind::SpawnBatch if safe_read_only_spawn => {
                        "Start trusted read-only agent work".to_owned()
                    }
                    AgentToolKind::Spawn | AgentToolKind::SpawnBatch => {
                        "Start agent work with effects requiring explicit review".to_owned()
                    }
                    AgentToolKind::Wait => "Wait for one agent state change".to_owned(),
                    AgentToolKind::ReadResult => "Read one bounded result page".to_owned(),
                    AgentToolKind::List => "Read the bounded agent thread list".to_owned(),
                    AgentToolKind::Cancel => "Cancel one active agent thread".to_owned(),
                    AgentToolKind::Message => "Route one message to an active agent".to_owned(),
                    AgentToolKind::Close => "Close one terminal agent thread".to_owned(),
                },
                step_count: 1,
                workspace_code_steps: 0,
            },
        })
    }

    async fn preview(&self, _ctx: ToolContext, args: Value) -> Result<Option<ToolPreview>> {
        Ok(match self.kind {
            AgentToolKind::RequestDelegation => Some(self.delegation_request_preview(&args)?),
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
    fn permission_inputs(
        &self,
        args: &Value,
    ) -> Result<(Vec<ToolSubject>, Option<sigil_kernel::ApprovalMode>, bool)> {
        let disabled = self.surface.multi_agent_mode == MultiAgentMode::None;
        match self.kind {
            AgentToolKind::RequestDelegation => {
                let parsed = RequestAgentDelegationArgs::parse(args)?;
                Ok((
                    parsed
                        .members
                        .into_iter()
                        .map(|member| {
                            ToolSubject::agent(member.spawn.profile_id.as_str().to_owned())
                        })
                        .collect(),
                    Some(if disabled {
                        sigil_kernel::ApprovalMode::Deny
                    } else {
                        sigil_kernel::ApprovalMode::Ask
                    }),
                    false,
                ))
            }
            AgentToolKind::Spawn => {
                let profile_id = AgentProfileId::new(required_string(args, "profile_id")?)?;
                let safe_read_only_spawn = self.safe_model_spawn_for_profile(&profile_id);
                Ok((
                    vec![ToolSubject::agent(profile_id.as_str().to_owned())],
                    Some(if disabled {
                        sigil_kernel::ApprovalMode::Deny
                    } else if safe_read_only_spawn {
                        sigil_kernel::ApprovalMode::Allow
                    } else {
                        sigil_kernel::ApprovalMode::Ask
                    }),
                    safe_read_only_spawn,
                ))
            }
            AgentToolKind::SpawnBatch => {
                let parsed = SpawnAgentsArgs::parse(args)?;
                let safe_read_only_spawn = parsed
                    .members
                    .iter()
                    .all(|member| self.safe_model_spawn_for_profile(&member.spawn.profile_id));
                Ok((
                    parsed
                        .members
                        .into_iter()
                        .map(|member| {
                            ToolSubject::agent(member.spawn.profile_id.as_str().to_owned())
                        })
                        .collect(),
                    Some(if disabled {
                        sigil_kernel::ApprovalMode::Deny
                    } else if safe_read_only_spawn {
                        sigil_kernel::ApprovalMode::Allow
                    } else {
                        sigil_kernel::ApprovalMode::Ask
                    }),
                    safe_read_only_spawn,
                ))
            }
            AgentToolKind::List => Ok((
                vec![ToolSubject::agent("all".to_owned())],
                Some(sigil_kernel::ApprovalMode::Allow),
                false,
            )),
            AgentToolKind::Wait
            | AgentToolKind::ReadResult
            | AgentToolKind::Cancel
            | AgentToolKind::Message
            | AgentToolKind::Close => Ok((
                vec![ToolSubject::agent(required_string(args, "thread_id")?)],
                Some(sigil_kernel::ApprovalMode::Allow),
                false,
            )),
        }
    }

    fn safe_model_spawn_for_profile(&self, profile_id: &AgentProfileId) -> bool {
        let Some(resolved) = self.surface.profile_registry.get(profile_id) else {
            return false;
        };
        let resolved_contracts = self
            .surface
            .base_tool_contracts
            .iter()
            .filter(|contract| resolved.profile.tool_scope.allows(&contract.spec.name))
            .cloned()
            .collect::<Vec<_>>();
        resolved.effective_enabled()
            && resolved.trust_state == AgentTrustState::Trusted
            && resolved.effective_model_invocation_allowed()
            && tool_contracts_are_safe_readonly_for_auto_spawn(&resolved_contracts)
    }

    fn description(&self) -> String {
        match self.kind {
            AgentToolKind::RequestDelegation => format!(
                "Propose one bounded batch of 1-4 child agents when you infer from the current ordinary conversation that delegation would materially improve the result. You own the intent decision and scope decomposition; the host does not scan prompt keywords. Calling this tool only requests a preview. The host validates the exact profiles, isolation, tool/effect upper bounds, and asks the user once. Children start only after that explicit confirmation. A denial returns control to this conversation with zero child dispatch. Use non-overlapping scopes and completion_mode=join_before_final when the next answer depends on the results.\n{}",
                self.surface.profile_index_description
            ),
            AgentToolKind::Spawn => format!(
                "{}\n{}",
                spawn_agent_mode_description(self.surface.multi_agent_mode),
                self.surface.profile_index_description
            ),
            AgentToolKind::SpawnBatch => format!(
                "Spawn 2-4 independent read-only child agents as one batch. The host preflights the whole batch, reserves every runtime slot atomically, and runs accepted members concurrently. completion_mode=join_before_final resumes the parent with bounded results without wait_agent polling. completion_mode=background returns immediately; live progress and result-ready continuation are host-driven, so do not poll. Every member must use a trusted model-visible profile whose effective tool contracts are proven read-only. Use this only for non-overlapping delegated scopes.\n{}",
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
            AgentToolKind::RequestDelegation => json!({
                "type": "object",
                "properties": {
                    "members": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 4,
                        "items": {
                            "type": "object",
                            "properties": {
                                "request_key": {
                                    "type": "string",
                                    "description": "Stable unique id for this proposed member."
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
                        "enum": ["join_before_final", "background"],
                        "default": "join_before_final",
                        "description": "Join the exact confirmed batch before the next model turn, or detach a proven-safe read-only batch."
                    }
                },
                "required": ["members"],
                "additionalProperties": false
            }),
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
                        "enum": ["join_before_final", "background"],
                        "default": "join_before_final",
                        "description": "Join before the next parent model turn, or detach the whole batch and let the host surface result-ready completion."
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

    fn delegation_request_preview(&self, args: &Value) -> Result<ToolPreview> {
        let parsed = RequestAgentDelegationArgs::parse(args)?;
        let mut rows = Vec::with_capacity(parsed.members.len());
        for member in &parsed.members {
            let resolved = self
                .surface
                .profile_registry
                .get(&member.spawn.profile_id)
                .with_context(|| {
                    format!(
                        "agent profile {} is not registered",
                        member.spawn.profile_id.as_str()
                    )
                })?;
            let isolation = if profile_uses_changeset_only_write(resolved.execution_role, resolved)
            {
                "changeset_only"
            } else {
                "shared_read_only"
            };
            let expected_effect = if isolation == "changeset_only" {
                "isolated changeset proposal; parent workspace unchanged"
            } else {
                "read-only workspace inspection"
            };
            rows.push(format!(
                "{}: profile={} · objective={} · isolation={} · tools={} · effect={} · trust={}",
                member.request_key.as_str(),
                member.spawn.profile_id.as_str(),
                member.spawn.objective,
                isolation,
                tool_scope_summary(&resolved.profile.tool_scope),
                expected_effect,
                resolved.trust_state_string(),
            ));
        }
        Ok(ToolPreview {
            title: format!("Delegate to {} agent(s)", parsed.members.len()),
            summary: format!(
                "{} · exact batch · user confirmation required",
                invocation_mode_label(parsed.completion_mode)
            ),
            body: rows.join("\n"),
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
                "{} read-only agents · {}",
                parsed.members.len(),
                invocation_mode_label(parsed.completion_mode)
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
    pub(super) completion_mode: AgentInvocationMode,
    pub(super) members: Vec<SpawnAgentsMemberArgs>,
}

pub(super) struct SpawnAgentsMemberArgs {
    pub(super) request_key: AgentRouteId,
    pub(super) spawn: SpawnAgentArgs,
    pub(super) raw_args: Value,
}

pub(super) struct RequestAgentDelegationArgs {
    pub(super) completion_mode: AgentInvocationMode,
    pub(super) members: Vec<SpawnAgentsMemberArgs>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestAgentDelegationWire {
    members: Vec<RequestAgentDelegationMemberWire>,
    #[serde(default)]
    completion_mode: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestAgentDelegationMemberWire {
    request_key: String,
    profile_id: String,
    objective: String,
    prompt: String,
    #[serde(default)]
    display_name_hint: Option<String>,
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
        if !matches!(
            completion_mode,
            AgentInvocationMode::JoinBeforeFinal | AgentInvocationMode::Background
        ) {
            bail!("spawn_agents completion_mode must be join_before_final or background");
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
                mode: completion_mode,
                display_name_hint: optional_string(member, "display_name_hint"),
            };
            parsed.push(SpawnAgentsMemberArgs {
                request_key,
                spawn,
                raw_args: member.clone(),
            });
        }
        parsed.sort_by(|left, right| left.request_key.cmp(&right.request_key));
        Ok(Self {
            completion_mode,
            members: parsed,
        })
    }
}

impl RequestAgentDelegationArgs {
    pub(super) fn parse(args: &Value) -> Result<Self> {
        let wire = serde_json::from_value::<RequestAgentDelegationWire>(args.clone())
            .context("invalid request_agent_delegation proposal")?;
        if !(1..=4).contains(&wire.members.len()) {
            bail!("request_agent_delegation requires between 1 and 4 members");
        }
        let completion_mode = wire
            .completion_mode
            .as_deref()
            .map(parse_invocation_mode)
            .transpose()?
            .unwrap_or(AgentInvocationMode::JoinBeforeFinal);
        if !matches!(
            completion_mode,
            AgentInvocationMode::JoinBeforeFinal | AgentInvocationMode::Background
        ) {
            bail!(
                "request_agent_delegation completion_mode must be join_before_final or background"
            );
        }

        let mut request_keys = BTreeSet::new();
        let mut members = Vec::with_capacity(wire.members.len());
        for member in wire.members {
            let request_key = AgentRouteId::new(member.request_key)?;
            if !request_keys.insert(request_key.clone()) {
                bail!(
                    "request_agent_delegation contains duplicate request_key {}",
                    request_key.as_str()
                );
            }
            let profile_id = AgentProfileId::new(member.profile_id)?;
            validate_proposal_text(
                "objective",
                &member.objective,
                MAX_DELEGATION_OBJECTIVE_CHARS,
            )?;
            validate_proposal_text("prompt", &member.prompt, MAX_DELEGATION_PROMPT_CHARS)?;
            if let Some(display_name_hint) = member.display_name_hint.as_deref() {
                validate_proposal_text(
                    "display_name_hint",
                    display_name_hint,
                    MAX_DELEGATION_DISPLAY_NAME_CHARS,
                )?;
            }
            let raw_args = json!({
                "request_key": request_key.as_str(),
                "profile_id": profile_id.as_str(),
                "objective": member.objective,
                "prompt": member.prompt,
                "display_name_hint": member.display_name_hint,
            });
            let spawn = SpawnAgentArgs {
                profile_id,
                objective: required_string(&raw_args, "objective")?,
                prompt: required_string(&raw_args, "prompt")?,
                mode: completion_mode,
                display_name_hint: optional_string(&raw_args, "display_name_hint"),
            };
            members.push(SpawnAgentsMemberArgs {
                request_key,
                spawn,
                raw_args,
            });
        }
        members.sort_by(|left, right| left.request_key.cmp(&right.request_key));
        Ok(Self {
            completion_mode,
            members,
        })
    }

    pub(super) fn single_spawn_args(&self) -> Option<Value> {
        let member = self.members.first()?;
        (self.members.len() == 1).then(|| {
            json!({
                "profile_id": member.spawn.profile_id.as_str(),
                "objective": member.spawn.objective,
                "prompt": member.spawn.prompt,
                "mode": invocation_mode_label(self.completion_mode),
                "display_name_hint": member.spawn.display_name_hint,
            })
        })
    }

    pub(super) fn batch_spawn_args(&self) -> Value {
        json!({
            "completion_mode": invocation_mode_label(self.completion_mode),
            "members": self.members.iter().map(|member| member.raw_args.clone()).collect::<Vec<_>>(),
        })
    }
}

fn validate_proposal_text(label: &str, value: &str, max_chars: usize) -> Result<()> {
    if value.trim().is_empty() {
        bail!("request_agent_delegation {label} cannot be empty");
    }
    if value.chars().count() > max_chars {
        bail!("request_agent_delegation {label} exceeds {max_chars} characters");
    }
    if value.chars().any(char::is_control) {
        bail!("request_agent_delegation {label} contains control characters");
    }
    Ok(())
}
