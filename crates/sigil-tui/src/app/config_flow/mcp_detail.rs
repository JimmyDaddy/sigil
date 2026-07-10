use super::*;

pub(super) fn render_mcp_lifecycle_summary(
    config_state: &ConfigState,
    runtime_status: &str,
) -> Vec<String> {
    let config = config_state
        .draft
        .base_root_config
        .mcp_servers
        .get(config_state.selected_mcp_server_index)
        .cloned()
        .unwrap_or_default();

    let (environment_grants, environment_live) = config_state
        .selected_mcp_server()
        .map(|server| mcp_environment_summary(&server.inherit_env))
        .unwrap_or_else(|| ("none".to_owned(), "unavailable".to_owned()));

    vec![
        render_config_readonly_row("Runtime", runtime_status),
        render_config_readonly_row("Required", bool_summary(config.required)),
        render_config_readonly_row("Startup", config.startup.as_str()),
        render_config_readonly_row("Trust", config.trust.trust_class.as_str()),
        render_config_readonly_row("Approval", config.trust.approval_default.as_str()),
        render_config_readonly_row("Pin", mcp_pin_summary(&config)),
        render_config_readonly_row("Environment", "isolated (env_clear)"),
        render_config_readonly_row("Inherited env", &environment_grants),
        render_config_readonly_row("Live fingerprint", &environment_live),
        render_config_readonly_row(
            "Secrets",
            if config.trust.allow_secrets {
                "allowed"
            } else {
                "blocked"
            },
        ),
    ]
}

fn mcp_environment_summary(names: &[String]) -> (String, String) {
    let grants = if names.is_empty() {
        "none".to_owned()
    } else {
        names.join(", ")
    };
    match sigil_kernel::resolve_extension_process_environment(names) {
        Ok(environment) => (
            grants,
            format!(
                "ready {}",
                environment
                    .live_fingerprint()
                    .chars()
                    .take(24)
                    .collect::<String>()
            ),
        ),
        Err(error) => (grants, format!("missing ({})", error.code.as_str())),
    }
}

pub(super) fn mcp_pin_summary(config: &McpServerConfig) -> &'static str {
    if !config.trust.pin_version {
        "off"
    } else if config.trust.pinned.is_some() {
        "pinned"
    } else {
        "missing"
    }
}
