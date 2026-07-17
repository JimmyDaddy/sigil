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

    let mut rows = vec![
        render_config_readonly_row("Runtime", runtime_status),
        render_config_readonly_row("Origin", "user root"),
        render_config_readonly_row("Transport", config.transport_name()),
        render_config_readonly_row("Required", bool_summary(config.required)),
        render_config_readonly_row("Startup", config.startup.as_str()),
        render_config_readonly_row("Trust", config.trust.trust_class.as_str()),
        render_config_readonly_row("Source policy", config.trust.approval_default.as_str()),
        render_config_readonly_row("Tool local access", "read"),
        render_config_readonly_row(
            "Tool network effect",
            if config.streamable_http().is_some() {
                "read"
            } else {
                "unknown"
            },
        ),
        render_config_readonly_row("Pin", mcp_pin_summary(&config)),
        render_config_readonly_row(
            "Secrets",
            if config.trust.allow_secrets {
                "allowed"
            } else {
                "blocked"
            },
        ),
    ];
    match &config.transport {
        sigil_kernel::McpServerTransportConfig::Stdio {
            command,
            inherit_env,
            ..
        } => {
            let (environment_grants, environment_live) = mcp_environment_summary(inherit_env);
            rows.push(render_config_readonly_row("Command", command));
            rows.push(render_config_readonly_row(
                "Server launch",
                "execute · network unknown",
            ));
            rows.push(render_config_readonly_row(
                "Environment",
                "isolated (env_clear)",
            ));
            rows.push(render_config_readonly_row(
                "Inherited env",
                &environment_grants,
            ));
            rows.push(render_config_readonly_row(
                "Live fingerprint",
                &environment_live,
            ));
        }
        sigil_kernel::McpServerTransportConfig::StreamableHttp(remote) => {
            rows.push(render_config_readonly_row(
                "Destination",
                &safe_remote_destination(&remote.url),
            ));
            rows.push(render_config_readonly_row(
                "Client capabilities",
                &remote_capability_summary(remote),
            ));
            rows.push(render_config_readonly_row(
                "Header sources",
                &remote_header_source_summary(remote),
            ));
            rows.push(render_config_readonly_row(
                "Credential env",
                &remote_environment_summary(remote),
            ));
            rows.push(render_config_readonly_row(
                "OAuth",
                &remote
                    .oauth
                    .as_ref()
                    .map(|oauth| {
                        let client = oauth.client_id.as_deref().unwrap_or("dynamic registration");
                        let scopes = crate::app::compact_mcp_oauth_scopes(&oauth.scopes);
                        format!("{client} · scopes {scopes} · system credential store")
                    })
                    .unwrap_or_else(|| "off".to_owned()),
            ));
            rows.push(render_config_readonly_row(
                "Static fingerprint",
                &sigil_runtime::mcp_transport_static_fingerprint(&config)
                    .unwrap_or_else(|_| "invalid".to_owned()),
            ));
            rows.push(render_config_readonly_row(
                "Live fingerprint",
                "activation only",
            ));
        }
    }
    rows
}

fn safe_remote_destination(value: &str) -> String {
    let Some((scheme, remainder)) = value.split_once("://") else {
        return "invalid".to_owned();
    };
    let authority = remainder.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() {
        "invalid".to_owned()
    } else {
        format!("{scheme}://{authority}/")
    }
}

fn remote_capability_summary(config: &sigil_kernel::McpStreamableHttpConfig) -> String {
    let capabilities = config
        .client_capabilities
        .iter()
        .map(|capability| match capability {
            sigil_kernel::McpRemoteClientCapability::Roots => "roots",
            sigil_kernel::McpRemoteClientCapability::ElicitationForm => "elicitation",
        })
        .collect::<Vec<_>>();
    if capabilities.is_empty() {
        "none".to_owned()
    } else {
        capabilities.join(", ")
    }
}

fn remote_header_source_summary(config: &sigil_kernel::McpStreamableHttpConfig) -> String {
    let sources = config
        .http_headers
        .keys()
        .map(|name| format!("{name}:literal"))
        .chain(
            config
                .env_http_headers
                .iter()
                .map(|(name, environment)| format!("{name}:env({environment})")),
        )
        .chain(
            config
                .bearer_token_env_var
                .iter()
                .map(|environment| format!("Authorization:bearer_env({environment})")),
        )
        .collect::<Vec<_>>();
    if sources.is_empty() {
        "none".to_owned()
    } else {
        sources.join(", ")
    }
}

fn remote_environment_summary(config: &sigil_kernel::McpStreamableHttpConfig) -> String {
    let names = config
        .env_http_headers
        .values()
        .chain(config.bearer_token_env_var.iter())
        .cloned()
        .collect::<Vec<_>>();
    if names.is_empty() {
        return "none".to_owned();
    }
    let missing = names
        .iter()
        .filter(|name| std::env::var_os(name).is_none())
        .cloned()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        format!("ready ({})", names.join(", "))
    } else {
        format!("missing ({})", missing.join(", "))
    }
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
