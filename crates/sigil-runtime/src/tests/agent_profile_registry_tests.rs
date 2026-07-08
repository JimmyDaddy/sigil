use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use anyhow::Result;
use serde_json::json;
use sigil_kernel::{
    AgentConfig, AgentInvocationPolicy, AgentProfileId, AgentProfilePolicyEntry,
    AgentProfileSource, AgentProfileTrustEntry, AgentResultPolicy, AgentTrustState, ApprovalMode,
    ControlEntry, MemoryConfig, PermissionConfig, PermissionPolicy, PermissionRule,
    PluginTrustDecision, PluginTrustEntry, RootConfig, SessionConfig, SessionLogEntry,
    SkillDescriptor, SkillRunMode, SkillSource, SkillTrustState, TaskConfig, ToolAccess,
    ToolAllowlistConfig, ToolCategory, ToolPreviewCapability, ToolRegistryScope, ToolSpec,
    ToolSubject, WorkspaceConfig,
};

use super::{
    AgentProfileIndexContext, AgentProfileRegistry, BUILD_PROFILE_ID, EXPLORE_PROFILE_ID,
    NativeAgentProfileFormat, PLAN_PROFILE_ID, WORKER_PROFILE_ID, agent_profile_source_label,
    child_session_skill_profile, configured_dir, fallback_plugin_agent_id,
    markdown_agent_profile_wire, markdown_body_without_frontmatter,
    namespaced_plugin_agent_profile_id, parse_agent_kind, parse_bool, parse_invocation_policy,
    parse_reasoning_effort, parse_result_policy, parse_trust_state, plugin_agent_profile_format,
    plugin_agent_profile_from_raw, sorted_dir_entries, workspace_path,
};

fn root_config() -> RootConfig {
    RootConfig {
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        storage: Default::default(),
        session: SessionConfig {
            log_dir: Some(".sigil/sessions".to_owned()),
        },
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: Some(12),
            tool_timeout_secs: 45,
        },
        permission: PermissionConfig::default(),
        model_request: Default::default(),
        memory: MemoryConfig { enabled: true },
        skills: Default::default(),
        compaction: sigil_kernel::CompactionConfig::default(),
        code_intelligence: sigil_kernel::CodeIntelligenceConfig::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: TaskConfig::default(),
        providers: BTreeMap::from([(
            "deepseek".to_owned(),
            json!({
                "base_url": "https://example.com",
            }),
        )]),
        mcp_servers: Vec::new(),
    }
}

fn skill_descriptor(id: &str, entrypoint: impl Into<std::path::PathBuf>) -> SkillDescriptor {
    SkillDescriptor {
        id: id.to_owned(),
        name: id.to_owned(),
        description: "Descriptor helper.".to_owned(),
        when_to_use: None,
        root: ".sigil/skills".into(),
        entrypoint: entrypoint.into(),
        source: SkillSource::Workspace,
        sha256: "abc123".to_owned(),
        enabled: true,
        trust: SkillTrustState::Trusted,
        model_invocable: true,
        user_invocable: true,
        run_as: SkillRunMode::ChildSession,
        agent: None,
        argument_hint: None,
        allowed_tools: ToolRegistryScope::default(),
        disallowed_tools: ToolRegistryScope::default(),
        path_patterns: Vec::new(),
    }
}

fn permission_spec(name: &str, access: ToolAccess) -> ToolSpec {
    ToolSpec {
        name: name.to_owned(),
        description: name.to_owned(),
        input_schema: json!({"type":"object"}),
        category: ToolCategory::File,
        access,
        preview: ToolPreviewCapability::None,
    }
}

#[test]
fn registry_discovers_native_workspace_agent_toml_profiles() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let agent_dir = workspace.join(".sigil").join("agents").join("review");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("agent.toml"),
        r#"
description = "Workspace review agent."
instructions = "Read the workspace and report findings."
trust = "trusted"
invocation_policy = "model_allowed"
allowed_tools = ["grep", "read_file"]
nickname_candidates = ["Scout", "Review"]
aliases = ["repo-reviewer"]
slash_names = ["review-agent"]
"#,
    )?;

    let mut config = root_config();
    config.skills.compatibility_sources = vec!["claude".to_owned()];
    let registry = AgentProfileRegistry::from_root_config_with_workspace(&config, &workspace)?;
    let review = registry
        .get(&AgentProfileId::new("review")?)
        .expect("native workspace agent exists");

    assert_eq!(review.source, AgentProfileSource::Workspace);
    assert_eq!(review.trust_state, AgentTrustState::Trusted);
    assert_eq!(review.profile.description, "Workspace review agent.");
    assert_eq!(
        review.profile.instructions,
        "Read the workspace and report findings."
    );
    assert!(review.profile.tool_scope.allows("grep"));
    assert!(review.profile.tool_scope.allows("read_file"));
    assert!(!review.profile.tool_scope.allows("write_file"));
    assert_eq!(
        review.profile.invocation_policy,
        AgentInvocationPolicy::ModelAllowed
    );
    assert!(review.profile.model_invocation_allowed());
    assert_eq!(review.profile.nickname_candidates, vec!["Scout", "Review"]);
    assert_eq!(review.profile.aliases, vec!["repo-reviewer"]);
    assert_eq!(review.profile.slash_names, vec!["review-agent"]);
    assert!(registry.warnings().is_empty());

    let visible = registry.model_visible_index(&AgentProfileIndexContext::default())?;
    let ids = visible
        .entries
        .iter()
        .map(|entry| entry.profile_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec![EXPLORE_PROFILE_ID, "review", WORKER_PROFILE_ID]);
    Ok(())
}

#[test]
fn registry_derives_execution_role_from_profile_policy_not_profile_id() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    for (id, body) in [
        (
            "patcher",
            r#"
description = "Custom patching agent."
instructions = "Propose changes."
trust = "trusted"
invocation_policy = "model_allowed"
result_policy = "foreground_merge_required"
allowed_tools = ["grep"]
"#,
        ),
        (
            "writer-name-read-role",
            r#"
description = "Read role despite write-looking tools."
instructions = "Inspect only."
trust = "trusted"
invocation_policy = "model_allowed"
allowed_tools = ["write_file"]
"#,
        ),
    ] {
        let agent_dir = workspace.join(".sigil").join("agents").join(id);
        fs::create_dir_all(&agent_dir)?;
        fs::write(agent_dir.join("agent.toml"), body)?;
    }

    let registry =
        AgentProfileRegistry::from_root_config_with_workspace(&root_config(), &workspace)?;
    let patcher = registry
        .get(&AgentProfileId::new("patcher")?)
        .expect("patcher profile exists");
    let read_profile = registry
        .get(&AgentProfileId::new("writer-name-read-role")?)
        .expect("read profile exists");

    assert_eq!(
        patcher.execution_role,
        sigil_kernel::AgentRole::SubagentWrite
    );
    assert_eq!(
        patcher.profile.result_policy,
        AgentResultPolicy::ForegroundMergeRequired
    );
    assert_eq!(
        read_profile.execution_role,
        sigil_kernel::AgentRole::SubagentRead
    );
    assert_ne!(patcher.profile.id.as_str(), WORKER_PROFILE_ID);
    Ok(())
}

#[test]
fn registry_uses_fixed_sigil_project_assets_for_native_workspace_agents() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let agent_dir = workspace.join(".sigil").join("agents").join("review");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("agent.toml"),
        r#"
description = "Project asset review agent."
instructions = "Review repository changes from configured project assets."
trust = "trusted"
invocation_policy = "model_allowed"
"#,
    )?;
    let mut config = root_config();
    config.workspace.root = workspace.display().to_string();

    let registry = AgentProfileRegistry::from_root_config_with_workspace(&config, &workspace)?;
    let review = registry
        .get(&AgentProfileId::new("review")?)
        .expect("native workspace agent exists");

    assert_eq!(review.source, AgentProfileSource::Workspace);
    assert_eq!(review.profile.description, "Project asset review agent.");
    assert!(review.profile.model_invocation_allowed());
    assert!(registry.warnings().is_empty());
    Ok(())
}

#[test]
fn registry_discovers_native_workspace_agent_markdown_profiles() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let agent_dir = workspace.join(".sigil").join("agents").join("audit");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("AGENT.md"),
        r#"---
description: Markdown audit agent.
trust: trusted
invocation_policy: model_allowed
allowed_tools: [grep]
nickname_candidates:
  - Audit
  - Probe
aliases:
  - audit-reader
slash_names:
  - audit-agent
---
Use grep to audit the requested scope.
"#,
    )?;

    let mut config = root_config();
    config.skills.compatibility_sources = vec!["claude".to_owned()];
    let registry = AgentProfileRegistry::from_root_config_with_workspace(&config, &workspace)?;
    let audit = registry
        .get(&AgentProfileId::new("audit")?)
        .expect("markdown workspace agent exists");

    assert_eq!(audit.profile.description, "Markdown audit agent.");
    assert_eq!(
        audit.profile.instructions,
        "Use grep to audit the requested scope."
    );
    assert!(audit.profile.tool_scope.allows("grep"));
    assert!(!audit.profile.tool_scope.allows("read_file"));
    assert_eq!(audit.profile.nickname_candidates, vec!["Audit", "Probe"]);
    assert_eq!(audit.profile.aliases, vec!["audit-reader"]);
    assert_eq!(audit.profile.slash_names, vec!["audit-agent"]);
    assert!(audit.profile.model_invocation_allowed());
    Ok(())
}

#[test]
fn registry_disables_conflicting_agent_aliases_without_dropping_profiles() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    for (id, alias) in [
        ("review-a", "review"),
        ("review-b", "review"),
        ("review", "self"),
    ] {
        let agent_dir = workspace.join(".sigil").join("agents").join(id);
        fs::create_dir_all(&agent_dir)?;
        fs::write(
            agent_dir.join("agent.toml"),
            format!(
                r#"
description = "{id}."
instructions = "Inspect only."
trust = "trusted"
aliases = ["{alias}"]
"#
            ),
        )?;
    }

    let mut config = root_config();
    config.skills.compatibility_sources = vec!["claude".to_owned()];
    let registry = AgentProfileRegistry::from_root_config_with_workspace(&config, &workspace)?;

    assert!(registry.get(&AgentProfileId::new("review-a")?).is_some());
    assert!(registry.get(&AgentProfileId::new("review-b")?).is_some());
    assert!(registry.get(&AgentProfileId::new("review")?).is_some());
    assert!(
        registry
            .get(&AgentProfileId::new("review-a")?)
            .expect("profile should remain")
            .profile
            .aliases
            .is_empty()
    );
    assert!(
        registry
            .get(&AgentProfileId::new("review-b")?)
            .expect("profile should remain")
            .profile
            .aliases
            .is_empty()
    );
    assert!(registry.warnings().iter().any(|warning| {
        warning.contains("agent profile alias \"review\" is ambiguous")
            && warning.contains("review-a")
            && warning.contains("review-b")
            && warning.contains("review")
    }));
    Ok(())
}

#[test]
fn registry_normalizes_agent_alias_and_slash_name_edges() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let self_alias_dir = workspace.join(".sigil").join("agents").join("self-alias");
    fs::create_dir_all(&self_alias_dir)?;
    fs::write(
        self_alias_dir.join("agent.toml"),
        r#"
description = "Self alias."
instructions = "Inspect only."
trust = "trusted"
aliases = ["", " @reviewer ", "self-alias"]
slash_names = [" /self-alias ", "/review-agent"]
"#,
    )?;
    let slash_conflict_dir = workspace.join(".sigil").join("agents").join("review-agent");
    fs::create_dir_all(&slash_conflict_dir)?;
    fs::write(
        slash_conflict_dir.join("agent.toml"),
        r#"
description = "Slash conflict."
instructions = "Inspect only."
trust = "trusted"
"#,
    )?;

    let mut config = root_config();
    config.skills.compatibility_sources = vec!["claude".to_owned()];
    let registry = AgentProfileRegistry::from_root_config_with_workspace(&config, &workspace)?;
    let self_alias = registry
        .get(&AgentProfileId::new("self-alias")?)
        .expect("self-alias profile should remain");

    assert_eq!(self_alias.profile.aliases, vec!["reviewer", "self-alias"]);
    assert_eq!(self_alias.profile.slash_names, vec!["self-alias"]);
    assert!(registry.warnings().iter().any(|warning| {
        warning.contains("agent profile slash name \"review-agent\" is ambiguous")
            && warning.contains("self-alias")
            && warning.contains("review-agent")
    }));
    Ok(())
}

#[test]
fn registry_discovers_trusted_plugin_agent_profiles() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let plugin_root = workspace.join(".sigil").join("plugins").join("repo.review");
    let agent_dir = plugin_root.join("agents").join("reviewer");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("agent.toml"),
        r#"
description = "Plugin review agent."
instructions = "Review repository changes with grep."
trust = "trusted"
invocation_policy = "model_allowed"
allowed_tools = ["grep"]
aliases = ["plugin-reviewer"]
slash_names = ["plugin-review"]
"#,
    )?;
    fs::write(
        plugin_root.join("plugin.toml"),
        r#"id = "repo.review"
name = "Repository Review"
version = "0.1.0"

[[agents]]
path = "agents/reviewer/agent.toml"
"#,
    )?;
    let pending = crate::discover_workspace_plugins(&workspace, &[])?;
    let trust = SessionLogEntry::Control(ControlEntry::PluginTrustDecision(
        PluginTrustEntry::for_snapshot(&pending.manifests[0], PluginTrustDecision::Trusted, 42)?,
    ));

    let registry = AgentProfileRegistry::from_root_config_with_workspace_and_entries(
        &root_config(),
        &workspace,
        &[trust],
    )?;
    let profile_id = AgentProfileId::new("repo_review-reviewer")?;
    let profile = registry
        .get(&profile_id)
        .expect("trusted plugin agent profile should be registered");

    assert_eq!(
        profile.source,
        AgentProfileSource::Plugin {
            plugin_id: "repo.review".to_owned()
        }
    );
    assert_eq!(profile.trust_state, AgentTrustState::Trusted);
    assert!(profile.profile.tool_scope.allows("grep"));
    assert!(!profile.profile.tool_scope.allows("write_file"));
    assert_eq!(profile.profile.aliases, vec!["plugin-reviewer"]);
    assert_eq!(profile.profile.slash_names, vec!["plugin-review"]);

    let visible = registry.model_visible_index(&AgentProfileIndexContext::default())?;
    assert!(
        visible
            .entries
            .iter()
            .any(|entry| entry.profile_id == profile_id)
    );
    Ok(())
}

#[test]
fn registry_reports_plugin_agent_profile_warning_edges() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");

    let invalid_format_root = workspace.join(".sigil/plugins/invalid-format");
    fs::create_dir_all(invalid_format_root.join("agents"))?;
    fs::write(
        invalid_format_root.join("agents/reviewer.txt"),
        "not an agent",
    )?;
    fs::write(
        invalid_format_root.join("plugin.toml"),
        r#"id = "invalid-format"
name = "Invalid Format"
version = "0.1.0"

[[agents]]
path = "agents/reviewer.txt"
"#,
    )?;

    let id_mismatch_root = workspace.join(".sigil/plugins/id-mismatch");
    fs::create_dir_all(id_mismatch_root.join("agents/reviewer"))?;
    fs::write(
        id_mismatch_root.join("agents/reviewer/agent.toml"),
        r#"id = "other"
description = "Mismatched id."
"#,
    )?;
    fs::write(
        id_mismatch_root.join("plugin.toml"),
        r#"id = "id-mismatch"
name = "ID Mismatch"
version = "0.1.0"

[[agents]]
path = "agents/reviewer/agent.toml"
"#,
    )?;

    let duplicate_root = workspace.join(".sigil/plugins/duplicate");
    fs::create_dir_all(duplicate_root.join("agents/one"))?;
    fs::write(
        duplicate_root.join("agents/one/agent.toml"),
        r#"description = "First duplicate."
trust = "trusted"
"#,
    )?;
    fs::write(
        duplicate_root.join("agents/one.toml"),
        r#"description = "Second duplicate."
trust = "trusted"
"#,
    )?;
    fs::write(
        duplicate_root.join("plugin.toml"),
        r#"id = "duplicate"
name = "Duplicate"
version = "0.1.0"

[[agents]]
path = "agents/one/agent.toml"

[[agents]]
path = "agents/one.toml"
"#,
    )?;

    let invalid_fallback_root = workspace.join(".sigil/plugins/invalid-fallback");
    fs::create_dir_all(invalid_fallback_root.join("agents"))?;
    fs::write(
        invalid_fallback_root.join("agents/.toml"),
        r#"description = "Invalid fallback id."
trust = "trusted"
"#,
    )?;
    fs::write(
        invalid_fallback_root.join("plugin.toml"),
        r#"id = "invalid-fallback"
name = "Invalid Fallback"
version = "0.1.0"

[[agents]]
path = "agents/.toml"
"#,
    )?;

    let unreadable_root = workspace.join(".sigil/plugins/unreadable");
    fs::create_dir_all(unreadable_root.join("agents/unreadable/agent.toml"))?;
    fs::write(
        unreadable_root.join("plugin.toml"),
        r#"id = "unreadable"
name = "Unreadable"
version = "0.1.0"

[[agents]]
path = "agents/unreadable/agent.toml"
"#,
    )?;

    let pending = crate::discover_workspace_plugins(&workspace, &[])?;
    let entries = pending
        .manifests
        .iter()
        .map(|manifest| {
            PluginTrustEntry::for_snapshot(manifest, PluginTrustDecision::Trusted, 42)
                .map(|trust| SessionLogEntry::Control(ControlEntry::PluginTrustDecision(trust)))
        })
        .collect::<Result<Vec<_>>>()?;

    let registry = AgentProfileRegistry::from_root_config_with_workspace_and_entries(
        &root_config(),
        &workspace,
        &entries,
    )?;
    let warnings = registry.warnings().join("\n");

    assert!(warnings.contains("agents/reviewer.txt"));
    assert!(warnings.contains("must point to agent.toml"));
    assert!(warnings.contains("must match file-derived id"));
    assert!(warnings.contains("plugin agent profile id \"duplicate-one\""));
    assert!(warnings.contains("invalid plugin agent fallback id"));
    assert!(warnings.contains("failed to read plugin agent profile agents/unreadable/agent.toml"));
    Ok(())
}

#[test]
fn plugin_agent_profile_helpers_cover_markdown_and_id_edges() -> Result<()> {
    assert!(plugin_agent_profile_format(Path::new("agents/reviewer.txt")).is_err());
    assert_eq!(
        plugin_agent_profile_format(Path::new("agents/reviewer.md"))?,
        NativeAgentProfileFormat::Markdown
    );
    assert!(fallback_plugin_agent_id(Path::new(".toml")).is_err());
    let long_local_id = "a".repeat(96);
    let namespaced = namespaced_plugin_agent_profile_id("plugin.with.dot", &long_local_id)?;
    assert!(namespaced.as_str().len() <= 96);
    assert!(namespaced.as_str().starts_with("plugin-"));

    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let plugin_root = workspace.join(".sigil/plugins/pack");
    let entrypoint = plugin_root.join("agents/reviewer/AGENT.md");
    let profile = plugin_agent_profile_from_raw(
        &root_config(),
        &workspace,
        "pack",
        &plugin_root,
        &entrypoint,
        "reviewer",
        r#"---
description: Markdown plugin agent.
trust: trusted
allowed_tools: [grep]
---
Use grep only.
"#,
        NativeAgentProfileFormat::Markdown,
    )?;
    assert_eq!(profile.profile.id.as_str(), "pack-reviewer");
    assert_eq!(profile.profile.instructions, "Use grep only.");
    assert!(profile.profile.tool_scope.allows("grep"));
    assert_eq!(
        profile.source,
        AgentProfileSource::Plugin {
            plugin_id: "pack".to_owned()
        }
    );

    let mismatch = plugin_agent_profile_from_raw(
        &root_config(),
        &workspace,
        "pack",
        &plugin_root,
        &entrypoint,
        "reviewer",
        r#"id = "other""#,
        NativeAgentProfileFormat::Toml,
    )
    .expect_err("plugin profile id must match fallback id");
    assert!(mismatch.to_string().contains("file-derived id"));
    Ok(())
}

#[test]
fn registry_projects_claude_child_session_agents_as_compatibility_profiles() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let agents_dir = workspace.join(".claude").join("agents");
    fs::create_dir_all(&agents_dir)?;
    fs::write(
        agents_dir.join("reviewer.md"),
        r#"---
id: reviewer
name: Human Review
description: Claude reviewer agent.
trust: trusted
disable-model-invocation: false
allowed-tools: [grep]
when-to-use: Use for focused review.
---
Review code through grep only.
"#,
    )?;

    let mut config = root_config();
    config.skills.compatibility_sources = vec!["claude".to_owned()];
    let registry = AgentProfileRegistry::from_root_config_with_workspace(&config, &workspace)?;
    let reviewer = registry
        .get(&AgentProfileId::new("reviewer")?)
        .expect("claude compatibility agent exists");

    assert_eq!(
        reviewer.source,
        AgentProfileSource::Compatibility {
            provider: "claude".to_owned()
        }
    );
    assert_eq!(reviewer.trust_state, AgentTrustState::Trusted);
    assert_eq!(reviewer.profile.description, "Claude reviewer agent.");
    assert!(
        reviewer
            .profile
            .instructions
            .contains("Use for focused review.")
    );
    assert!(
        reviewer
            .profile
            .instructions
            .contains("Review code through grep only.")
    );
    assert!(reviewer.profile.tool_scope.allows("grep"));
    assert!(!reviewer.profile.tool_scope.allows("read_file"));
    assert_eq!(
        reviewer.profile.invocation_policy,
        AgentInvocationPolicy::ModelAllowed
    );
    assert!(reviewer.profile.model_invocation_allowed());
    assert_eq!(reviewer.profile.skills, vec!["reviewer"]);
    assert_eq!(reviewer.profile.nickname_candidates, vec!["Human Review"]);

    let visible = registry.model_visible_index(&AgentProfileIndexContext::default())?;
    let ids = visible
        .entries
        .iter()
        .map(|entry| entry.profile_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec![EXPLORE_PROFILE_ID, "reviewer", WORKER_PROFILE_ID]);
    Ok(())
}

#[test]
fn registry_projects_reasonix_agents_only_when_compatibility_source_is_enabled() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let agents_dir = workspace.join(".reasonix").join("agents");
    fs::create_dir_all(&agents_dir)?;
    fs::write(
        agents_dir.join("critic.md"),
        r#"---
id: critic
name: Critical Friend
description: Reasonix critic.
trust: trusted
runAs: subagent
disableModelInvocation: true
allowedTools: [grep]
---
Critique the target with read-only evidence.
"#,
    )?;

    let default_registry =
        AgentProfileRegistry::from_root_config_with_workspace(&root_config(), &workspace)?;
    assert!(
        default_registry
            .get(&AgentProfileId::new("critic")?)
            .is_none()
    );

    let mut config = root_config();
    config.skills.compatibility_sources = vec!["claude".to_owned(), "reasonix".to_owned()];
    let registry = AgentProfileRegistry::from_root_config_with_workspace(&config, &workspace)?;
    let critic = registry
        .get(&AgentProfileId::new("critic")?)
        .expect("reasonix compatibility agent exists after enabling source");

    assert_eq!(
        critic.source,
        AgentProfileSource::Compatibility {
            provider: "reasonix".to_owned()
        }
    );
    assert_eq!(critic.trust_state, AgentTrustState::Trusted);
    assert_eq!(critic.profile.description, "Reasonix critic.");
    assert!(
        critic
            .profile
            .instructions
            .contains("Critique the target with read-only evidence.")
    );
    assert!(critic.profile.tool_scope.allows("grep"));
    assert_eq!(
        critic.profile.invocation_policy,
        AgentInvocationPolicy::ManualOnly
    );
    assert!(critic.profile.user_invocation_allowed());
    assert!(!critic.profile.model_invocation_allowed());
    Ok(())
}

#[test]
fn registry_skips_child_session_skills_with_disallowed_tools() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let agents_dir = workspace.join(".claude").join("agents");
    fs::create_dir_all(&agents_dir)?;
    fs::write(
        agents_dir.join("narrow.md"),
        r#"---
description: Cannot be safely projected.
trust: trusted
disable-model-invocation: false
disallowed-tools: [write_file]
---
This compatibility agent subtracts tools.
"#,
    )?;

    let mut config = root_config();
    config.skills.compatibility_sources = vec!["claude".to_owned()];
    let registry = AgentProfileRegistry::from_root_config_with_workspace(&config, &workspace)?;
    assert!(registry.get(&AgentProfileId::new("narrow")?).is_none());
    assert!(registry.warnings().iter().any(|warning| {
        warning.contains("narrow")
            && warning.contains("disallowed_tools cannot be represented safely")
    }));
    Ok(())
}

#[test]
fn registry_keeps_native_workspace_agents_manual_and_untrusted_by_default() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let agent_dir = workspace.join(".sigil").join("agents").join("local");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("agent.toml"),
        r#"
description = "Local manual agent."
instructions = "Manual only."
"#,
    )?;

    let registry =
        AgentProfileRegistry::from_root_config_with_workspace(&root_config(), &workspace)?;
    let local = registry
        .get(&AgentProfileId::new("local")?)
        .expect("manual workspace agent exists");

    assert_eq!(local.trust_state, AgentTrustState::NeedsReview);
    assert_eq!(
        local.profile.invocation_policy,
        AgentInvocationPolicy::ManualOnly
    );
    assert!(local.profile.user_invocation_allowed());
    assert!(!local.profile.model_invocation_allowed());
    assert!(local.profile.tool_scope.allows("grep"));
    assert!(!local.profile.tool_scope.allows("write_file"));

    let visible = registry.model_visible_index(&AgentProfileIndexContext::default())?;
    let visible_ids = visible
        .entries
        .iter()
        .map(|entry| entry.profile_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(visible_ids, vec![EXPLORE_PROFILE_ID, WORKER_PROFILE_ID]);
    Ok(())
}

#[test]
fn registry_applies_durable_trust_and_invalidates_profile_hash_changes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let agent_dir = workspace.join(".sigil").join("agents").join("review");
    fs::create_dir_all(&agent_dir)?;
    let agent_file = agent_dir.join("agent.toml");
    fs::write(
        &agent_file,
        r#"
description = "Review agent."
instructions = "Review the workspace."
invocation_policy = "model_allowed"
allowed_tools = ["grep"]
"#,
    )?;

    let base_registry =
        AgentProfileRegistry::from_root_config_with_workspace(&root_config(), &workspace)?;
    let profile_id = AgentProfileId::new("review")?;
    let review = base_registry
        .get(&profile_id)
        .expect("workspace agent exists");
    assert_eq!(review.trust_state, AgentTrustState::NeedsReview);
    assert!(
        base_registry
            .model_visible_index(&AgentProfileIndexContext::default())?
            .entries
            .iter()
            .all(|entry| entry.profile_id.as_str() != profile_id.as_str())
    );
    let snapshot = base_registry.capture_snapshot(&profile_id)?;
    let entries = vec![SessionLogEntry::Control(
        ControlEntry::AgentProfileTrustDecision(AgentProfileTrustEntry {
            profile_id: profile_id.clone(),
            source: snapshot.source.clone(),
            source_hash: snapshot.source_hash.clone(),
            profile_hash: snapshot.profile_hash.clone(),
            decision: AgentTrustState::Trusted,
            reviewed_at_ms: 42,
        }),
    )];

    let trusted_registry = AgentProfileRegistry::from_root_config_with_workspace_and_entries(
        &root_config(),
        &workspace,
        &entries,
    )?;
    assert_eq!(
        trusted_registry
            .get(&profile_id)
            .expect("trusted workspace agent exists")
            .trust_state,
        AgentTrustState::Trusted
    );
    assert!(
        trusted_registry
            .model_visible_index(&AgentProfileIndexContext::default())?
            .entries
            .iter()
            .any(|entry| entry.profile_id.as_str() == profile_id.as_str())
    );

    fs::write(
        &agent_file,
        r#"
description = "Review agent."
instructions = "Review the workspace and summarize risks."
invocation_policy = "model_allowed"
allowed_tools = ["grep"]
"#,
    )?;
    let changed_registry = AgentProfileRegistry::from_root_config_with_workspace_and_entries(
        &root_config(),
        &workspace,
        &entries,
    )?;
    assert_eq!(
        changed_registry
            .get(&profile_id)
            .expect("changed workspace agent exists")
            .trust_state,
        AgentTrustState::NeedsReview
    );
    assert!(
        changed_registry
            .model_visible_index(&AgentProfileIndexContext::default())?
            .entries
            .iter()
            .all(|entry| entry.profile_id.as_str() != profile_id.as_str())
    );
    Ok(())
}

#[test]
fn registry_applies_durable_policy_without_mutating_source_profile() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let agent_dir = workspace.join(".sigil").join("agents").join("review");
    fs::create_dir_all(&agent_dir)?;
    let agent_file = agent_dir.join("agent.toml");
    fs::write(
        &agent_file,
        r#"
description = "Review agent."
instructions = "Review the workspace."
invocation_policy = "model_allowed"
allowed_tools = ["grep"]
"#,
    )?;

    let profile_id = AgentProfileId::new("review")?;
    let base_registry =
        AgentProfileRegistry::from_root_config_with_workspace(&root_config(), &workspace)?;
    let snapshot = base_registry.capture_snapshot(&profile_id)?;
    let trust_entry = SessionLogEntry::Control(ControlEntry::AgentProfileTrustDecision(
        AgentProfileTrustEntry {
            profile_id: profile_id.clone(),
            source: snapshot.source.clone(),
            source_hash: snapshot.source_hash.clone(),
            profile_hash: snapshot.profile_hash.clone(),
            decision: AgentTrustState::Trusted,
            reviewed_at_ms: 42,
        },
    ));

    let disabled_entries = vec![
        trust_entry.clone(),
        SessionLogEntry::Control(ControlEntry::AgentProfilePolicyDecision(
            AgentProfilePolicyEntry {
                profile_id: profile_id.clone(),
                source: snapshot.source.clone(),
                source_hash: snapshot.source_hash.clone(),
                profile_hash: snapshot.profile_hash.clone(),
                enabled: Some(false),
                user_invocable: None,
                model_invocable: None,
                reviewed_at_ms: 43,
            },
        )),
    ];
    let disabled_registry = AgentProfileRegistry::from_root_config_with_workspace_and_entries(
        &root_config(),
        &workspace,
        &disabled_entries,
    )?;
    let disabled_review = disabled_registry
        .get(&profile_id)
        .expect("disabled workspace agent exists");
    assert!(disabled_review.enabled);
    assert!(!disabled_review.effective_enabled());
    assert!(
        disabled_registry
            .model_visible_index(&AgentProfileIndexContext::default())?
            .entries
            .iter()
            .all(|entry| entry.profile_id.as_str() != profile_id.as_str())
    );

    let model_disabled_entries = vec![
        trust_entry,
        SessionLogEntry::Control(ControlEntry::AgentProfilePolicyDecision(
            AgentProfilePolicyEntry {
                profile_id: profile_id.clone(),
                source: snapshot.source.clone(),
                source_hash: snapshot.source_hash.clone(),
                profile_hash: snapshot.profile_hash.clone(),
                enabled: None,
                user_invocable: None,
                model_invocable: Some(false),
                reviewed_at_ms: 44,
            },
        )),
    ];
    let model_disabled_registry =
        AgentProfileRegistry::from_root_config_with_workspace_and_entries(
            &root_config(),
            &workspace,
            &model_disabled_entries,
        )?;
    let model_disabled_review = model_disabled_registry
        .get(&profile_id)
        .expect("model-disabled workspace agent exists");
    assert!(model_disabled_review.profile.model_invocation_allowed());
    assert!(!model_disabled_review.effective_model_invocation_allowed());
    assert!(
        model_disabled_registry
            .model_visible_index(&AgentProfileIndexContext::default())?
            .entries
            .iter()
            .all(|entry| entry.profile_id.as_str() != profile_id.as_str())
    );

    fs::write(
        &agent_file,
        r#"
description = "Review agent."
instructions = "Review the workspace and summarize risks."
invocation_policy = "model_allowed"
allowed_tools = ["grep"]
"#,
    )?;
    let changed_base =
        AgentProfileRegistry::from_root_config_with_workspace(&root_config(), &workspace)?;
    let changed_snapshot = changed_base.capture_snapshot(&profile_id)?;
    let changed_trust = SessionLogEntry::Control(ControlEntry::AgentProfileTrustDecision(
        AgentProfileTrustEntry {
            profile_id: profile_id.clone(),
            source: changed_snapshot.source.clone(),
            source_hash: changed_snapshot.source_hash.clone(),
            profile_hash: changed_snapshot.profile_hash.clone(),
            decision: AgentTrustState::Trusted,
            reviewed_at_ms: 45,
        },
    ));
    let stale_policy_entries = vec![
        changed_trust,
        SessionLogEntry::Control(ControlEntry::AgentProfilePolicyDecision(
            AgentProfilePolicyEntry {
                profile_id: profile_id.clone(),
                source: snapshot.source,
                source_hash: snapshot.source_hash,
                profile_hash: snapshot.profile_hash,
                enabled: None,
                user_invocable: None,
                model_invocable: Some(false),
                reviewed_at_ms: 44,
            },
        )),
    ];
    let changed_registry = AgentProfileRegistry::from_root_config_with_workspace_and_entries(
        &root_config(),
        &workspace,
        &stale_policy_entries,
    )?;
    let changed_review = changed_registry
        .get(&profile_id)
        .expect("changed workspace agent exists");
    assert_eq!(changed_review.trust_state, AgentTrustState::Trusted);
    assert!(changed_review.effective_model_invocation_allowed());
    assert!(
        changed_registry
            .model_visible_index(&AgentProfileIndexContext::default())?
            .entries
            .iter()
            .any(|entry| entry.profile_id.as_str() == profile_id.as_str())
    );
    Ok(())
}

#[test]
fn registry_rejects_duplicate_native_workspace_agent_ids() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let agent_dir = workspace
        .join(".sigil")
        .join("agents")
        .join(EXPLORE_PROFILE_ID);
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("agent.toml"),
        r#"
description = "Duplicate built-in id."
trust = "trusted"
invocation_policy = "model_allowed"
"#,
    )?;

    let registry =
        AgentProfileRegistry::from_root_config_with_workspace(&root_config(), &workspace)?;
    assert!(
        registry
            .warnings()
            .iter()
            .any(|warning| warning.contains("shadowed"))
    );
    let profiles = registry
        .profiles()
        .iter()
        .filter(|profile| profile.profile.id.as_str() == EXPLORE_PROFILE_ID)
        .count();
    assert_eq!(profiles, 1);
    Ok(())
}

#[cfg(unix)]
#[test]
fn registry_rejects_workspace_agent_symlink_escape() -> Result<()> {
    use std::os::unix::fs as unix_fs;

    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let outside = temp.path().join("outside-agent");
    fs::create_dir_all(workspace.join(".sigil").join("agents"))?;
    fs::create_dir_all(&outside)?;
    fs::write(
        outside.join("agent.toml"),
        r#"
description = "Escaped agent."
trust = "trusted"
invocation_policy = "model_allowed"
"#,
    )?;
    unix_fs::symlink(
        &outside,
        workspace.join(".sigil").join("agents").join("escape"),
    )?;

    let registry =
        AgentProfileRegistry::from_root_config_with_workspace(&root_config(), &workspace)?;
    assert!(registry.get(&AgentProfileId::new("escape")?).is_none());
    assert!(
        registry
            .warnings()
            .iter()
            .any(|warning| warning.contains("escapes workspace root"))
    );

    let linked_entry_dir = workspace.join(".sigil").join("agents").join("linked-entry");
    fs::create_dir_all(&linked_entry_dir)?;
    fs::write(
        outside.join("AGENT.md"),
        "---\ntrust: trusted\n---\nOutside",
    )?;
    unix_fs::symlink(outside.join("AGENT.md"), linked_entry_dir.join("AGENT.md"))?;
    let registry =
        AgentProfileRegistry::from_root_config_with_workspace(&root_config(), &workspace)?;
    assert!(
        registry
            .get(&AgentProfileId::new("linked-entry")?)
            .is_none()
    );
    assert!(registry.warnings().iter().any(|warning| {
        warning.contains("workspace agent profile entrypoint escapes workspace root")
    }));
    Ok(())
}

#[test]
fn registry_reports_workspace_agent_discovery_edge_warnings() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace)?;

    let mut config = root_config();
    config.skills.enabled = false;
    let disabled_registry =
        AgentProfileRegistry::from_root_config_with_workspace(&config, &workspace)?;
    assert!(disabled_registry.warnings().is_empty());

    let config = root_config();
    fs::create_dir_all(workspace.join(".sigil"))?;
    fs::write(workspace.join(".sigil").join("agents"), "not a directory")?;
    let file_registry = AgentProfileRegistry::from_root_config_with_workspace(&config, &workspace)?;
    assert!(
        file_registry.warnings().iter().any(|warning| {
            warning.contains("workspace agent discovery path is not a directory")
        })
    );

    fs::remove_file(workspace.join(".sigil").join("agents"))?;
    let agents_dir = workspace.join(".sigil").join("agents");
    fs::create_dir_all(&agents_dir)?;
    fs::write(agents_dir.join("noise.txt"), "ignored")?;
    fs::create_dir_all(agents_dir.join("empty"))?;
    let invalid_dir = agents_dir.join("bad id");
    fs::create_dir_all(&invalid_dir)?;
    let invalid_profile_dir = agents_dir.join("broken");
    fs::create_dir_all(&invalid_profile_dir)?;
    fs::write(
        invalid_profile_dir.join("agent.toml"),
        r#"
id = "other"
description = "Invalid id"
"#,
    )?;
    let warning_registry =
        AgentProfileRegistry::from_root_config_with_workspace(&root_config(), &workspace)?;
    assert!(
        warning_registry
            .warnings()
            .iter()
            .any(|warning| { warning.contains("invalid workspace agent directory name") })
    );
    assert!(
        warning_registry
            .warnings()
            .iter()
            .any(|warning| { warning.contains("invalid workspace agent profile") })
    );

    let duplicate_skill = workspace
        .join(".sigil")
        .join("skills")
        .join(EXPLORE_PROFILE_ID);
    fs::create_dir_all(&duplicate_skill)?;
    fs::write(
        duplicate_skill.join("SKILL.md"),
        r#"---
id: explore
run-as: child-session
trust: trusted
---
Duplicate built-in profile.
"#,
    )?;
    let duplicate_registry =
        AgentProfileRegistry::from_root_config_with_workspace(&root_config(), &workspace)?;
    assert!(duplicate_registry.warnings().iter().any(|warning| {
        warning.contains("child-session skill agent profile id") && warning.contains("shadowed")
    }));
    Ok(())
}

#[test]
fn registry_projects_existing_task_roles_to_builtin_profiles() -> Result<()> {
    let mut config = root_config();
    config.task.subagent_read.provider = Some("anthropic".to_owned());
    config.task.subagent_read.model = Some("claude-opus".to_owned());
    config.task.subagent_read.tools = ToolAllowlistConfig {
        allow_all: false,
        names: vec!["grep".to_owned(), "read_file".to_owned()],
        prefixes: Vec::new(),
    };

    let registry = AgentProfileRegistry::from_root_config(&config)?;
    let ids = registry
        .profiles()
        .iter()
        .map(|profile| profile.profile.id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        ids,
        vec![
            BUILD_PROFILE_ID,
            EXPLORE_PROFILE_ID,
            PLAN_PROFILE_ID,
            WORKER_PROFILE_ID
        ]
    );
    let explore = registry
        .profiles()
        .iter()
        .find(|profile| profile.profile.id.as_str() == EXPLORE_PROFILE_ID)
        .expect("explore profile exists");
    assert_eq!(explore.id().as_str(), EXPLORE_PROFILE_ID);
    assert_eq!(explore.profile.provider.as_deref(), Some("anthropic"));
    assert_eq!(explore.profile.model.as_deref(), Some("claude-opus"));
    assert!(explore.profile.tool_scope.allows("grep"));
    assert!(!explore.profile.tool_scope.allows("write_file"));
    assert!(explore.profile.user_invocable);
    assert!(explore.profile.model_invocable);
    assert_eq!(
        explore.profile.invocation_policy,
        AgentInvocationPolicy::ModelAllowed
    );
    assert_eq!(
        explore.profile.result_policy,
        AgentResultPolicy::SummaryWithPageRef
    );
    let worker = registry
        .profiles()
        .iter()
        .find(|profile| profile.profile.id.as_str() == WORKER_PROFILE_ID)
        .expect("worker profile exists");
    assert_eq!(
        worker.profile.result_policy,
        AgentResultPolicy::ForegroundMergeRequired
    );
    assert_eq!(
        worker.execution_role,
        sigil_kernel::AgentRole::SubagentWrite
    );
    assert_eq!(
        worker.profile.invocation_policy,
        AgentInvocationPolicy::ModelAllowed
    );
    assert!(worker.profile.model_invocation_allowed());
    assert!(!worker.profile.tool_scope.allows("write_file"));
    assert!(!worker.profile.tool_scope.allows("apply_changeset"));
    assert!(!worker.profile.tool_scope.allows("bash"));
    assert!(registry.warnings().is_empty());
    Ok(())
}

#[test]
fn registry_markdown_frontmatter_parser_covers_aliases_and_errors() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let agents_dir = workspace.join(".sigil").join("agents");
    let rich_dir = agents_dir.join("rich");
    fs::create_dir_all(&rich_dir)?;
    fs::write(
        rich_dir.join("AGENT.md"),
        r#"---
kind: system
description: "Quoted description"
reasoning-effort: max
invocation-policy: system
result-policy: artifact
enabled: yes
trust-state: disabled
user-invocable: no
model-invocable: no
tools:
  - 'grep'
skills: [alpha, "beta"]
mcp-servers: ['filesystem']
nickname-candidates:
  - "Rich"
---
Use rich metadata.
"#,
    )?;
    let registry =
        AgentProfileRegistry::from_root_config_with_workspace(&root_config(), &workspace)?;
    let rich = registry
        .get(&AgentProfileId::new("rich")?)
        .expect("rich profile is discovered");
    assert_eq!(rich.profile.kind, sigil_kernel::AgentProfileKind::System);
    assert_eq!(rich.profile.description, "Quoted description");
    assert_eq!(
        rich.profile.reasoning_effort,
        Some(sigil_kernel::ReasoningEffort::Max)
    );
    assert_eq!(
        rich.profile.invocation_policy,
        AgentInvocationPolicy::SystemOnly
    );
    assert_eq!(rich.profile.result_policy, AgentResultPolicy::ArtifactOnly);
    assert!(rich.enabled);
    assert_eq!(rich.trust_state, AgentTrustState::Disabled);
    assert!(rich.profile.tool_scope.allows("grep"));
    assert_eq!(rich.profile.skills, vec!["alpha", "beta"]);
    assert_eq!(rich.profile.mcp_servers, vec!["filesystem"]);
    assert_eq!(rich.profile.nickname_candidates, vec!["Rich"]);

    let no_frontmatter_dir = agents_dir.join("plain");
    fs::create_dir_all(&no_frontmatter_dir)?;
    fs::write(no_frontmatter_dir.join("AGENT.md"), "Plain body only.")?;
    let plain_registry =
        AgentProfileRegistry::from_root_config_with_workspace(&root_config(), &workspace)?;
    let plain = plain_registry
        .get(&AgentProfileId::new("plain")?)
        .expect("plain markdown profile is discovered");
    assert_eq!(plain.profile.instructions, "Plain body only.");

    let invalid_dir = agents_dir.join("bad-list");
    fs::create_dir_all(&invalid_dir)?;
    fs::write(
        invalid_dir.join("AGENT.md"),
        r#"---
- orphan
---
bad
"#,
    )?;
    let invalid_registry =
        AgentProfileRegistry::from_root_config_with_workspace(&root_config(), &workspace)?;
    assert!(
        invalid_registry
            .warnings()
            .iter()
            .any(|warning| { warning.contains("frontmatter list item without a key") })
    );

    let unterminated_dir = agents_dir.join("unterminated");
    fs::create_dir_all(&unterminated_dir)?;
    fs::write(
        unterminated_dir.join("AGENT.md"),
        r#"---
description: Missing close
"#,
    )?;
    let unterminated_registry =
        AgentProfileRegistry::from_root_config_with_workspace(&root_config(), &workspace)?;
    assert!(
        unterminated_registry
            .warnings()
            .iter()
            .any(|warning| { warning.contains("unterminated agent frontmatter") })
    );
    Ok(())
}

#[test]
fn registry_toml_agent_permission_overrides_root_policy() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let agent_dir = workspace.join(".sigil").join("agents").join("writer");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("agent.toml"),
        r#"
description = "Workspace writer"
trust = "trusted"

[permission]
edit = "allow"

[permission.bash]
"*" = "ask"
"cargo test *" = "allow"
"git push*" = "deny"
"#,
    )?;
    let mut config = root_config();
    config.permission.rules.push(PermissionRule {
        tool_name: Some("write_file".to_owned()),
        subject_glob: None,
        mode: ApprovalMode::Deny,
    });

    let registry = AgentProfileRegistry::from_root_config_with_workspace(&config, &workspace)?;
    let writer = registry
        .get(&AgentProfileId::new("writer")?)
        .expect("writer profile is discovered");
    let write_decision = PermissionPolicy::new(&writer.profile.permission_policy).decide(
        &permission_spec("write_file", ToolAccess::Write),
        "write_file",
        vec![ToolSubject::path("crates/lib.rs", "crates/lib.rs")],
    )?;
    let test_decision = PermissionPolicy::new(&writer.profile.permission_policy).decide(
        &permission_spec("bash", ToolAccess::Execute),
        "bash",
        vec![ToolSubject::command(
            "cargo test -p sigil-runtime",
            "cargo test -p sigil-runtime",
        )],
    )?;
    let push_decision = PermissionPolicy::new(&writer.profile.permission_policy).decide(
        &permission_spec("bash", ToolAccess::Execute),
        "bash",
        vec![ToolSubject::command(
            "git push origin main",
            "git push origin main",
        )],
    )?;

    assert_eq!(write_decision.mode, ApprovalMode::Allow);
    assert_eq!(test_decision.mode, ApprovalMode::Allow);
    assert_eq!(push_decision.mode, ApprovalMode::Deny);
    Ok(())
}

#[test]
fn registry_markdown_agent_permission_nested_maps_are_parsed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let agent_dir = workspace.join(".sigil").join("agents").join("reviewer");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("AGENT.md"),
        r#"---
description: "Markdown reviewer"
trust: trusted
permission:
  edit:
    "*": ask
    "docs/**": allow
  bash:
    "*": ask
    "git diff *": allow
    "git push*": deny
---
Review and edit docs.
"#,
    )?;
    let registry =
        AgentProfileRegistry::from_root_config_with_workspace(&root_config(), &workspace)?;
    let reviewer = registry
        .get(&AgentProfileId::new("reviewer")?)
        .expect("reviewer profile is discovered");
    let docs_write = PermissionPolicy::new(&reviewer.profile.permission_policy).decide(
        &permission_spec("edit_file", ToolAccess::Write),
        "edit_file",
        vec![ToolSubject::path("docs/guide.md", "docs/guide.md")],
    )?;
    let src_write = PermissionPolicy::new(&reviewer.profile.permission_policy).decide(
        &permission_spec("edit_file", ToolAccess::Write),
        "edit_file",
        vec![ToolSubject::path("src/lib.rs", "src/lib.rs")],
    )?;
    let git_diff = PermissionPolicy::new(&reviewer.profile.permission_policy).decide(
        &permission_spec("bash", ToolAccess::Execute),
        "bash",
        vec![ToolSubject::command(
            "git diff -- crates",
            "git diff -- crates",
        )],
    )?;

    assert_eq!(docs_write.mode, ApprovalMode::Allow);
    assert_eq!(src_write.mode, ApprovalMode::Ask);
    assert_eq!(git_diff.mode, ApprovalMode::Allow);
    Ok(())
}

#[test]
fn registry_toml_agent_permission_commands_are_parsed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let agent_dir = workspace.join(".sigil").join("agents").join("shell-runner");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("agent.toml"),
        r#"
description = "Shell runner"
trust = "trusted"

[permission.commands]
allow = ["git status*", "git diff*"]
ask = ["cargo test -p sigil-runtime*"]
deny = ["git push*"]
"#,
    )?;

    let registry =
        AgentProfileRegistry::from_root_config_with_workspace(&root_config(), &workspace)?;
    let runner = registry
        .get(&AgentProfileId::new("shell-runner")?)
        .expect("shell-runner profile is discovered");
    let status = PermissionPolicy::new(&runner.profile.permission_policy).decide(
        &permission_spec("bash", ToolAccess::Execute),
        "bash",
        vec![ToolSubject::command(
            "git status --short",
            "family:git_read_only",
        )],
    )?;
    let test = PermissionPolicy::new(&runner.profile.permission_policy).decide(
        &permission_spec("bash", ToolAccess::Execute),
        "bash",
        vec![ToolSubject::command(
            "cargo test -p sigil-runtime",
            "family:cargo_test",
        )],
    )?;
    let push = PermissionPolicy::new(&runner.profile.permission_policy).decide(
        &permission_spec("bash", ToolAccess::Execute),
        "bash",
        vec![ToolSubject::command(
            "git push origin main",
            "git push origin main",
        )],
    )?;

    assert_eq!(runner.profile.permission_policy.commands.pattern_count(), 4);
    assert_eq!(status.mode, ApprovalMode::Allow);
    assert_eq!(test.mode, ApprovalMode::Ask);
    assert_eq!(push.mode, ApprovalMode::Deny);
    Ok(())
}

#[test]
fn registry_markdown_agent_permission_commands_are_parsed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let agent_dir = workspace
        .join(".sigil")
        .join("agents")
        .join("markdown-shell");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("AGENT.md"),
        r#"---
description: "Markdown shell"
trust: trusted
permission:
  commands:
    allow:
      - "git status*"
    ask: ["cargo test -p sigil-runtime*"]
    deny:
      - "git push*"
---
Run selected commands.
"#,
    )?;

    let registry =
        AgentProfileRegistry::from_root_config_with_workspace(&root_config(), &workspace)?;
    let runner = registry
        .get(&AgentProfileId::new("markdown-shell")?)
        .expect("markdown-shell profile is discovered");
    let status = PermissionPolicy::new(&runner.profile.permission_policy).decide(
        &permission_spec("bash", ToolAccess::Execute),
        "bash",
        vec![ToolSubject::command(
            "git status --short",
            "family:git_read_only",
        )],
    )?;
    let test = PermissionPolicy::new(&runner.profile.permission_policy).decide(
        &permission_spec("bash", ToolAccess::Execute),
        "bash",
        vec![ToolSubject::command(
            "cargo test -p sigil-runtime",
            "family:cargo_test",
        )],
    )?;
    let push = PermissionPolicy::new(&runner.profile.permission_policy).decide(
        &permission_spec("bash", ToolAccess::Execute),
        "bash",
        vec![ToolSubject::command(
            "git push origin main",
            "git push origin main",
        )],
    )?;

    assert_eq!(runner.profile.permission_policy.commands.pattern_count(), 3);
    assert_eq!(status.mode, ApprovalMode::Allow);
    assert_eq!(test.mode, ApprovalMode::Ask);
    assert_eq!(push.mode, ApprovalMode::Deny);
    Ok(())
}

#[test]
fn registry_from_entries_and_child_skill_helpers_cover_projection_edges() -> Result<()> {
    let registry = AgentProfileRegistry::from_root_config_with_entries(&root_config(), &[])?;
    assert!(
        registry
            .get(&AgentProfileId::new(EXPLORE_PROFILE_ID)?)
            .is_some()
    );

    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let relative_entrypoint = workspace.join(".sigil/skills/local/SKILL.md");
    fs::create_dir_all(relative_entrypoint.parent().expect("skill parent"))?;
    fs::write(
        &relative_entrypoint,
        "---\ndescription: Local\n---\nUse local skill.",
    )?;
    let mut local = skill_descriptor("local", ".sigil/skills/local/SKILL.md");
    local.name.clear();
    local.trust = SkillTrustState::NeedsReview;
    let local_profile = child_session_skill_profile(&root_config(), &workspace, &local)?;
    assert!(local_profile.profile.tool_scope.allows("grep"));
    assert!(local_profile.profile.nickname_candidates.is_empty());
    assert_eq!(local_profile.source, AgentProfileSource::Workspace);
    assert_eq!(local_profile.trust_state, AgentTrustState::NeedsReview);

    let missing = skill_descriptor("missing", ".sigil/skills/missing/SKILL.md");
    let error = child_session_skill_profile(&root_config(), &workspace, &missing)
        .expect_err("missing child-session entrypoint is rejected");
    assert!(
        error
            .to_string()
            .contains("failed to read child-session skill")
    );

    let absolute_entrypoint = workspace.join(".sigil/skills/user/SKILL.md");
    fs::create_dir_all(absolute_entrypoint.parent().expect("skill parent"))?;
    fs::write(&absolute_entrypoint, "User body")?;
    let mut user = skill_descriptor("user", absolute_entrypoint.clone());
    user.source = SkillSource::User;
    user.trust = SkillTrustState::Disabled;
    user.allowed_tools =
        ToolRegistryScope::from_names_and_prefixes(["grep"], std::iter::empty::<&str>());
    let user_profile = child_session_skill_profile(&root_config(), &workspace, &user)?;
    assert_eq!(user_profile.source, AgentProfileSource::User);
    assert_eq!(user_profile.trust_state, AgentTrustState::Disabled);
    assert_eq!(
        workspace_path(&workspace, &absolute_entrypoint),
        absolute_entrypoint
    );

    let mut plugin = skill_descriptor("plugin", ".sigil/skills/local/SKILL.md");
    plugin.source = SkillSource::Plugin {
        plugin_id: "plug".to_owned(),
    };
    let plugin_profile = child_session_skill_profile(&root_config(), &workspace, &plugin)?;
    assert_eq!(
        plugin_profile.source,
        AgentProfileSource::Plugin {
            plugin_id: "plug".to_owned()
        }
    );

    let labels = [
        (AgentProfileSource::User, "user"),
        (
            AgentProfileSource::Plugin {
                plugin_id: "plug".to_owned(),
            },
            "plugin",
        ),
        (
            AgentProfileSource::Compatibility {
                provider: "claude".to_owned(),
            },
            "compatibility",
        ),
        (AgentProfileSource::Unknown, "unknown"),
    ];
    for (source, expected) in labels {
        assert_eq!(agent_profile_source_label(&source), expected);
    }
    Ok(())
}

#[test]
fn registry_private_parser_helpers_cover_error_and_path_edges() -> Result<()> {
    assert_eq!(
        markdown_body_without_frontmatter("plain body"),
        "plain body"
    );
    assert_eq!(
        markdown_body_without_frontmatter("---\r\nname: a\r\n---\r\nBody"),
        "Body"
    );
    assert_eq!(
        markdown_body_without_frontmatter("---\nname: a"),
        "---\nname: a"
    );
    let (empty_wire, empty_body) = markdown_agent_profile_wire("")?;
    assert!(empty_wire.id.is_none());
    assert!(empty_body.is_none());
    let (comment_wire, comment_body) =
        markdown_agent_profile_wire("---\n# comment\n\nid: commented\n---\nBody")?;
    assert_eq!(comment_wire.id.as_deref(), Some("commented"));
    assert_eq!(comment_body.as_deref(), Some("Body"));
    assert!(markdown_agent_profile_wire("---\nnot valid\n---\n").is_err());

    assert!(!parse_bool("FALSE")?);
    assert!(parse_bool("maybe").is_err());
    assert_eq!(
        parse_agent_kind("primary")?,
        sigil_kernel::AgentProfileKind::Primary
    );
    assert!(parse_agent_kind("bogus").is_err());
    assert_eq!(
        parse_invocation_policy("manual")?,
        AgentInvocationPolicy::ManualOnly
    );
    assert!(parse_invocation_policy("bogus").is_err());
    assert_eq!(
        parse_result_policy("foreground")?,
        AgentResultPolicy::ForegroundMergeRequired
    );
    assert!(parse_result_policy("bogus").is_err());
    assert_eq!(parse_trust_state("review")?, AgentTrustState::NeedsReview);
    assert!(parse_trust_state("bogus").is_err());
    assert_eq!(
        parse_reasoning_effort("high")?,
        sigil_kernel::ReasoningEffort::High
    );
    assert!(parse_reasoning_effort("bogus").is_err());

    let temp = tempfile::tempdir()?;
    let absolute = temp.path().join("agents");
    assert_eq!(
        configured_dir(temp.path(), &absolute.display().to_string()),
        absolute
    );
    let mut warnings = Vec::new();
    assert!(sorted_dir_entries(&temp.path().join("missing"), &mut warnings).is_empty());
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("failed to read workspace agent discovery directory"))
    );
    Ok(())
}

#[test]
fn registry_model_visible_index_is_deterministic_and_fingerprinted() -> Result<()> {
    let registry = AgentProfileRegistry::from_root_config(&root_config())?;
    let context = AgentProfileIndexContext::default();

    let first = registry.model_visible_index(&context)?;
    let second = registry.model_visible_index(&context)?;

    assert_eq!(first, second);
    assert_eq!(first.entries.len(), 2);
    assert_eq!(first.entries[0].profile_id.as_str(), EXPLORE_PROFILE_ID);
    assert_eq!(
        first.entries[0].result_policy,
        AgentResultPolicy::SummaryWithPageRef
    );
    assert!(!first.fingerprint.is_empty());

    let mut truncated_context = context;
    truncated_context.max_entries = Some(0);
    let truncated = registry.model_visible_index(&truncated_context)?;
    assert!(truncated.entries.is_empty());
    assert_eq!(truncated.hidden_count, 2);
    assert_ne!(first.fingerprint, truncated.fingerprint);
    Ok(())
}

#[test]
fn registry_filters_untrusted_or_disabled_model_invocable_profiles() -> Result<()> {
    let mut registry = AgentProfileRegistry::from_root_config(&root_config())?;
    let explore_id = AgentProfileId::new(EXPLORE_PROFILE_ID)?;
    let context = AgentProfileIndexContext {
        allowed_profile_ids: Some(BTreeSet::from([explore_id.clone()])),
        ..AgentProfileIndexContext::default()
    };

    registry
        .profiles
        .iter_mut()
        .find(|profile| profile.profile.id == explore_id)
        .expect("explore profile exists")
        .enabled = false;
    assert!(registry.model_visible_index(&context)?.entries.is_empty());

    {
        let explore = registry
            .profiles
            .iter_mut()
            .find(|profile| profile.profile.id == explore_id)
            .expect("explore profile exists");
        explore.enabled = true;
        explore.trust_state = AgentTrustState::NeedsReview;
    }
    assert!(registry.model_visible_index(&context)?.entries.is_empty());

    registry
        .profiles
        .iter_mut()
        .find(|profile| profile.profile.id == explore_id)
        .expect("explore profile exists")
        .trust_state = AgentTrustState::Trusted;
    let scoped_context = AgentProfileIndexContext {
        allowed_profile_ids: Some(BTreeSet::new()),
        ..AgentProfileIndexContext::default()
    };
    assert!(
        registry
            .model_visible_index(&scoped_context)?
            .entries
            .is_empty()
    );
    Ok(())
}

#[test]
fn registry_filters_model_visible_index_by_tool_scope() -> Result<()> {
    let mut registry = AgentProfileRegistry::from_root_config(&root_config())?;
    let explore_id = AgentProfileId::new(EXPLORE_PROFILE_ID)?;
    let explore = registry
        .profiles
        .iter_mut()
        .find(|profile| profile.profile.id == explore_id)
        .expect("explore profile exists");
    explore.profile.tool_scope = sigil_kernel::ToolRegistryScope {
        allow_all: true,
        ..sigil_kernel::ToolRegistryScope::default()
    };
    let read_only_context = AgentProfileIndexContext {
        tool_scope: sigil_kernel::ToolRegistryScope::from_names_and_prefixes(
            ["grep"],
            std::iter::empty::<&str>(),
        ),
        ..AgentProfileIndexContext::default()
    };
    assert!(
        registry
            .model_visible_index(&read_only_context)?
            .entries
            .is_empty()
    );

    let explore = registry
        .profiles
        .iter_mut()
        .find(|profile| profile.profile.id == explore_id)
        .expect("explore profile exists");
    explore.profile.tool_scope =
        sigil_kernel::ToolRegistryScope::from_names_and_prefixes(["grep"], ["mcp__filesystem__"]);
    let scoped_context = AgentProfileIndexContext {
        tool_scope: sigil_kernel::ToolRegistryScope::from_names_and_prefixes(["grep"], ["mcp__"]),
        ..AgentProfileIndexContext::default()
    };
    let visible = registry.model_visible_index(&scoped_context)?;
    assert_eq!(visible.entries.len(), 1);
    assert_eq!(visible.entries[0].profile_id.as_str(), EXPLORE_PROFILE_ID);
    Ok(())
}
