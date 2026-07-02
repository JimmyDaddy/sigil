use super::*;

/// Builds shared agent run options for CLI, TUI, and future entrypoints.
pub fn build_run_options(
    root_config: &RootConfig,
    workspace_root: PathBuf,
    interaction_mode: InteractionMode,
) -> AgentRunOptions {
    let workspace_root = canonical_workspace_root(workspace_root);
    let paths = resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace_root);
    AgentRunOptions {
        traffic_partition_key: Some(workspace_partition_key(&workspace_root)),
        workspace_root,
        max_turns: root_config.agent.max_turns,
        tool_timeout_secs: root_config.agent.tool_timeout_secs,
        reasoning_effort: Some(default_reasoning_effort(root_config)),
        interaction_mode,
        permission_config: root_config.permission.clone(),
        permission_context: permission_evaluation_context(root_config, &paths),
        memory_config: root_config.memory.clone(),
        compaction_config: root_config.compaction.clone(),
    }
}

fn permission_evaluation_context(
    root_config: &RootConfig,
    paths: &SigilPaths,
) -> PermissionEvaluationContext {
    PermissionEvaluationContext {
        workspace_root: paths.workspace_root.clone(),
        project_asset_roots: vec![
            paths.project_assets_root.clone(),
            project_asset_dir(
                &paths.workspace_root,
                &paths.project_assets_root,
                &root_config.skills.workspace_dir,
                DEFAULT_WORKSPACE_SKILLS_DIR,
                "skills",
            ),
            project_asset_dir(
                &paths.workspace_root,
                &paths.project_assets_root,
                &root_config.skills.workspace_agents_dir,
                DEFAULT_WORKSPACE_AGENTS_DIR,
                "agents",
            ),
            paths.project_assets_root.join("plugins"),
        ],
        runtime_state_roots: vec![
            paths.workspace_state_root.clone(),
            paths.session_log_dir.clone(),
            paths.input_history_file.clone(),
            paths.artifacts_root.clone(),
            paths.changesets_root.clone(),
            paths.terminal_tasks_root.clone(),
        ],
        user_state_roots: vec![paths.state_root.clone()],
        user_cache_roots: vec![paths.cache_root.clone(), paths.workspace_cache_root.clone()],
        effective_policy_cap: None,
    }
}

/// Builds shared agent run options for one task role.
pub fn build_role_run_options(
    root_config: &RootConfig,
    workspace_root: PathBuf,
    interaction_mode: InteractionMode,
    role: AgentRole,
) -> AgentRunOptions {
    let mut options = build_run_options(root_config, workspace_root, interaction_mode);
    if let Some(reasoning_effort) = root_config.task.role_config(role).reasoning_effort.clone() {
        options.reasoning_effort = Some(reasoning_effort);
    }
    options
}

/// Builds a role-scoped tool registry view over an existing runtime registry.
pub fn build_role_tool_registry(
    registry: &ToolRegistry,
    root_config: &RootConfig,
    role: AgentRole,
) -> ScopedToolRegistry {
    registry.scoped(role_tool_scope(root_config, role))
}

/// Builds the tool registry used by plan-mode prompts.
///
/// Plan mode uses planner-scoped tools for read-only exploration while keeping agent-thread tools
/// visible so explicit delegation can still run through the same child-session contract as chat.
pub fn build_plan_prompt_tool_registry(
    registry: &ToolRegistry,
    root_config: &RootConfig,
) -> ScopedToolRegistry {
    registry.scoped(role_tool_scope(root_config, AgentRole::Planner).union(&agent_tool_scope()))
}

/// Builds the current agent registry further constrained by a loaded skill descriptor.
pub fn build_skill_tool_registry(
    registry: &ToolRegistry,
    skill: &SkillDescriptor,
) -> ScopedToolRegistry {
    let effective_scope = if skill.allowed_tools.is_empty() {
        ToolRegistryScope {
            allow_all: true,
            ..ToolRegistryScope::default()
        }
    } else {
        skill.allowed_tools.clone()
    };
    registry.scoped_with_denies(
        effective_scope,
        skill.disallowed_tools.union(&agent_tool_deny_scope()),
    )
}

/// Builds a role-scoped registry further constrained by a loaded skill descriptor.
pub fn build_role_skill_tool_registry(
    registry: &ToolRegistry,
    root_config: &RootConfig,
    role: AgentRole,
    skill: &SkillDescriptor,
) -> ScopedToolRegistry {
    let role_scope = role_tool_scope(root_config, role);
    let effective_scope = if skill.allowed_tools.is_empty() {
        role_scope
    } else {
        role_scope.intersection(&skill.allowed_tools)
    };
    registry.scoped_with_denies(
        effective_scope,
        skill.disallowed_tools.union(&agent_tool_deny_scope()),
    )
}

fn agent_tool_deny_scope() -> ToolRegistryScope {
    agent_tool_scope()
}

fn agent_tool_scope() -> ToolRegistryScope {
    ToolRegistryScope::from_names_and_prefixes(
        [
            agent_tools::SPAWN_AGENT_TOOL_NAME,
            agent_tools::WAIT_AGENT_TOOL_NAME,
            agent_tools::READ_AGENT_RESULT_TOOL_NAME,
            agent_tools::MESSAGE_AGENT_TOOL_NAME,
            agent_tools::CLOSE_AGENT_TOOL_NAME,
        ],
        std::iter::empty::<&str>(),
    )
}

fn role_tool_scope(root_config: &RootConfig, role: AgentRole) -> ToolRegistryScope {
    let configured = &root_config.task.role_config(role).tools;
    if configured_allowlist_is_empty(configured) {
        default_role_tool_scope(root_config, role)
    } else {
        tool_scope_from_allowlist(configured)
    }
}

pub(super) fn canonical_workspace_root(workspace_root: PathBuf) -> PathBuf {
    workspace_root.canonicalize().unwrap_or(workspace_root)
}

fn default_reasoning_effort(root_config: &RootConfig) -> ReasoningEffort {
    if provider_config_key(&root_config.agent.provider) == "deepseek"
        && let Ok(config) = load_deepseek_config(root_config)
    {
        return config.profile().default_reasoning_effort;
    }
    ReasoningEffort::Max
}

fn configured_allowlist_is_empty(config: &ToolAllowlistConfig) -> bool {
    !config.allow_all && config.names.is_empty() && config.prefixes.is_empty()
}

fn tool_scope_from_allowlist(config: &ToolAllowlistConfig) -> ToolRegistryScope {
    ToolRegistryScope {
        allow_all: config.allow_all,
        names: config.names.iter().cloned().collect(),
        prefixes: config.prefixes.clone(),
    }
}

fn default_role_tool_scope(root_config: &RootConfig, role: AgentRole) -> ToolRegistryScope {
    match role {
        AgentRole::Planner | AgentRole::SubagentRead => read_only_role_tool_scope(),
        AgentRole::Executor => ToolRegistryScope {
            allow_all: true,
            ..ToolRegistryScope::default()
        },
        AgentRole::SubagentWrite if root_config.task.allow_write_subagents => ToolRegistryScope {
            allow_all: true,
            ..ToolRegistryScope::default()
        },
        AgentRole::SubagentWrite => read_only_role_tool_scope(),
    }
}

fn read_only_role_tool_scope() -> ToolRegistryScope {
    ToolRegistryScope::from_names_and_prefixes(
        [
            "read_file",
            "ls",
            "glob",
            "grep",
            "code_symbols",
            "code_workspace_symbols",
            "code_definition",
            "code_references",
            "code_diagnostics",
            LOAD_SKILL_TOOL_NAME,
        ],
        std::iter::empty::<&str>(),
    )
}

fn workspace_partition_key(workspace_root: &std::path::Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(workspace_root.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    format!("workspace-{digest:x}")
}
