use super::*;

fn profile(
    id: &str,
    kind: AgentProfileKind,
    source: AgentProfileSource,
    description: &str,
) -> ResolvedAgentProfile {
    ResolvedAgentProfile {
        profile: sigil_kernel::AgentProfile {
            id: sigil_kernel::AgentProfileId::new(id).expect("agent id should parse"),
            kind,
            description: description.to_owned(),
            instructions: "inspect only".to_owned(),
            model: None,
            provider: None,
            reasoning_effort: None,
            tool_scope: Default::default(),
            permission_policy: Default::default(),
            invocation_policy: sigil_kernel::AgentInvocationPolicy::ManualOnly,
            result_policy: sigil_kernel::AgentResultPolicy::SummaryWithPageRef,
            user_invocable: true,
            model_invocable: true,
            skills: Vec::new(),
            mcp_servers: Vec::new(),
            nickname_candidates: vec!["repo scout".to_owned()],
            aliases: Vec::new(),
            slash_names: Vec::new(),
        },
        execution_role: sigil_kernel::AgentRole::SubagentRead,
        enabled: true,
        enabled_override: None,
        user_invocable_override: None,
        model_invocable_override: None,
        source,
        source_hash: "sha256:source".to_owned(),
        trust_state: AgentTrustState::Trusted,
    }
}

#[test]
fn agent_mention_errors_and_private_labels_cover_edge_paths() {
    assert_eq!(AppState::agent_mention_query("@review later"), None);

    let setup_app = AppState::from_setup("sigil.toml".into(), std::path::PathBuf::from("."), None);
    assert!(setup_app.user_invocable_agent_profiles().is_empty());
    assert!(
        setup_app
            .resolve_agent_mention_invocation("plain prompt")
            .expect_err("plain prompt should not be an agent mention")
            .to_string()
            .contains("not an agent mention")
    );
    assert_eq!(
        setup_app
            .resolve_agent_mention_invocation("@")
            .expect_err("empty mention should fail")
            .to_string(),
        "usage: @agent <prompt>"
    );
    assert_eq!(
        setup_app
            .resolve_agent_mention_invocation("@review")
            .expect_err("missing prompt should fail")
            .to_string(),
        "usage: @review <prompt>"
    );

    let user_profile = profile(
        "review",
        AgentProfileKind::Primary,
        AgentProfileSource::User,
        "",
    );
    assert!(agent_profile_matches_query(&user_profile, ""));
    assert!(agent_profile_matches_query(&user_profile, "repo scout"));
    assert_eq!(agent_mention_description(&user_profile), "primary · user");
    let mut aliased_user_profile = user_profile.clone();
    aliased_user_profile.profile.aliases = vec!["rr".to_owned()];
    assert_eq!(
        agent_mention_description(&aliased_user_profile),
        "primary · user · aliases: rr"
    );
    assert_eq!(agent_profile_kind_label(AgentProfileKind::System), "system");
    assert_eq!(
        agent_profile_kind_label(AgentProfileKind::Unknown),
        "unknown"
    );

    assert_eq!(
        agent_profile_source_label(&AgentProfileSource::Plugin {
            plugin_id: "pack".to_owned(),
        }),
        "plugin:pack"
    );
    assert_eq!(
        agent_profile_source_label(&AgentProfileSource::Compatibility {
            provider: "claude".to_owned(),
        }),
        "compat:claude"
    );
    assert_eq!(
        agent_profile_source_label(&AgentProfileSource::Unknown),
        "unknown"
    );

    let described = profile(
        "sub",
        AgentProfileKind::Subagent,
        AgentProfileSource::Workspace,
        "Review repository changes.",
    );
    assert_eq!(
        agent_mention_description(&described),
        "subagent · workspace · Review repository changes."
    );
}
