use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use serde_json::json;
use sigil_kernel::{
    AgentConfig, AgentProfileId, AgentTrustState, MemoryConfig, PermissionConfig, RootConfig,
    SessionConfig, TaskConfig, ToolAllowlistConfig, WorkspaceConfig,
};

use super::{
    AgentProfileIndexContext, AgentProfileRegistry, BUILD_PROFILE_ID, EXPLORE_PROFILE_ID,
    PLAN_PROFILE_ID, WORKER_PROFILE_ID,
};

fn root_config() -> RootConfig {
    RootConfig {
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        session: SessionConfig {
            log_dir: ".sigil/sessions".to_owned(),
        },
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: Some(12),
            tool_timeout_secs: 45,
        },
        permission: PermissionConfig::default(),
        memory: MemoryConfig { enabled: true },
        skills: Default::default(),
        compaction: sigil_kernel::CompactionConfig::default(),
        code_intelligence: sigil_kernel::CodeIntelligenceConfig::default(),
        terminal: Default::default(),
        task: TaskConfig::default(),
        providers: BTreeMap::from([(
            "deepseek".to_owned(),
            json!({
                "base_url": "https://example.com",
                "model": "deepseek-v4-flash",
            }),
        )]),
        mcp_servers: Vec::new(),
    }
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
    assert!(registry.warnings().is_empty());
    Ok(())
}

#[test]
fn registry_model_visible_index_is_deterministic_and_fingerprinted() -> Result<()> {
    let registry = AgentProfileRegistry::from_root_config(&root_config())?;
    let context = AgentProfileIndexContext::default();

    let first = registry.model_visible_index(&context)?;
    let second = registry.model_visible_index(&context)?;

    assert_eq!(first, second);
    assert_eq!(first.entries.len(), 1);
    assert_eq!(first.entries[0].profile_id.as_str(), EXPLORE_PROFILE_ID);
    assert!(!first.fingerprint.is_empty());

    let mut truncated_context = context;
    truncated_context.max_entries = Some(0);
    let truncated = registry.model_visible_index(&truncated_context)?;
    assert!(truncated.entries.is_empty());
    assert_eq!(truncated.hidden_count, 1);
    assert_ne!(first.fingerprint, truncated.fingerprint);
    Ok(())
}

#[test]
fn registry_filters_untrusted_or_disabled_model_invocable_profiles() -> Result<()> {
    let mut registry = AgentProfileRegistry::from_root_config(&root_config())?;
    let context = AgentProfileIndexContext::default();
    let explore_id = AgentProfileId::new(EXPLORE_PROFILE_ID)?;

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
