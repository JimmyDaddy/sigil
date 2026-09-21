use super::*;

#[test]
fn read_only_role_scope_keeps_its_default_filesystem_surface() {
    let scope = read_only_role_tool_scope();

    assert!(!scope.allow_all);
    assert!(scope.allows("read_file"));
    assert!(scope.allows("grep"));
    assert!(!scope.allows("write_file"));
}

#[tokio::test]
async fn plan_review_exposes_registered_tools_for_effect_based_adjudication() -> Result<()> {
    let mut registry = ToolRegistry::new();
    sigil_tools_builtin::register_builtin_tools(&mut registry);
    let config: RootConfig = toml::from_str(
        "config_version = 2\n[agent]\nconnection = \"deepseek-default\"\nmodel = \"deepseek-v4-flash\"\n",
    )?;
    register_agent_tools(&mut registry, &config)?;

    let plan_review = build_plan_review_tool_registry(&registry, &config);

    assert!(plan_review.spec_for("read_file").is_some());
    assert!(plan_review.spec_for("write_file").is_some());
    assert!(plan_review.spec_for(SPAWN_AGENT_TOOL_NAME).is_some());
    Ok(())
}

#[tokio::test]
async fn explicit_planner_allowlist_still_limits_plan_review_tool_visibility() -> Result<()> {
    let mut registry = ToolRegistry::new();
    sigil_tools_builtin::register_builtin_tools(&mut registry);
    let mut config: RootConfig = toml::from_str(
        "config_version = 2\n[agent]\nconnection = \"deepseek-default\"\nmodel = \"deepseek-v4-flash\"\n",
    )?;
    config.task.planner.tools = sigil_kernel::ToolAllowlistConfig {
        allow_all: false,
        names: vec!["grep".to_owned()],
        prefixes: Vec::new(),
    };

    let plan_review = build_plan_review_tool_registry(&registry, &config);

    assert!(plan_review.spec_for("grep").is_some());
    assert!(plan_review.spec_for("read_file").is_none());
    assert!(plan_review.spec_for("write_file").is_none());
    Ok(())
}
