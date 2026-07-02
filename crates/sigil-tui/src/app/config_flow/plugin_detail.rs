use super::*;

pub(super) fn render_plugin_detail_lines(plugin: &PluginManifestSnapshot) -> Vec<String> {
    let name = if plugin.name.trim().is_empty() {
        plugin.plugin_id.as_str()
    } else {
        plugin.name.as_str()
    };
    let description = plugin.description.as_deref().unwrap_or("none");
    let mut lines = vec![
        render_config_readonly_row("Name", name),
        render_config_readonly_row("Version", &plugin.version),
        render_config_readonly_row("Description", description),
        render_config_readonly_row("Trust", plugin.trust.as_str()),
        render_config_readonly_row("Manifest", &plugin.manifest_path.display().to_string()),
    ];
    push_wrapped_readonly_rows(&mut lines, "Hash", &plugin.manifest_hash);
    lines.push(render_config_readonly_row(
        "Implications",
        &plugin_implication_summary(&plugin.capabilities),
    ));
    lines.extend(render_plugin_agent_lines(&plugin.capabilities));
    lines.extend(render_plugin_skill_lines(&plugin.capabilities));
    lines.extend(render_plugin_hook_lines(&plugin.capabilities));
    lines.extend(render_plugin_mcp_lines(&plugin.capabilities));
    lines.push(render_config_readonly_row(
        "Approve",
        "trusts this reviewed manifest",
    ));
    lines.push(render_config_readonly_row(
        "Deny",
        "disables this reviewed manifest",
    ));
    lines
}

pub(super) fn render_plugin_index_lines(config_state: &ConfigState) -> Vec<String> {
    config_state
        .plugin_manifests
        .iter()
        .enumerate()
        .map(|(index, plugin)| {
            let marker = if index == config_state.selected_plugin_index {
                ">"
            } else {
                " "
            };
            format!(
                "{marker} {}: {} · {}",
                plugin.plugin_id,
                plugin.trust.as_str(),
                plugin.version
            )
        })
        .collect()
}

pub(super) fn plugin_implication_summary(capabilities: &[PluginCapability]) -> String {
    let mut parts = Vec::new();
    if capabilities
        .iter()
        .any(|capability| matches!(capability, PluginCapability::Agent { .. }))
    {
        parts.push("agent profiles");
    }
    if capabilities
        .iter()
        .any(|capability| matches!(capability, PluginCapability::Skill { .. }))
    {
        parts.push("skill instructions");
    }
    if capabilities
        .iter()
        .any(|capability| matches!(capability, PluginCapability::Hook { .. }))
    {
        parts.push("hook commands");
    }
    if capabilities
        .iter()
        .any(|capability| matches!(capability, PluginCapability::McpServer { .. }))
    {
        parts.push("MCP server processes");
    }
    if parts.is_empty() {
        "none".to_owned()
    } else {
        parts.join(", ")
    }
}

pub(super) fn render_plugin_agent_lines(capabilities: &[PluginCapability]) -> Vec<String> {
    let agents = capabilities
        .iter()
        .filter_map(|capability| match capability {
            PluginCapability::Agent { path } => Some(path.display().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut lines = vec![String::new(), "[agents]".to_owned()];
    if agents.is_empty() {
        lines.push(render_config_readonly_row("Agent count", "0"));
        return lines;
    }
    for (index, path) in agents.iter().enumerate() {
        push_wrapped_readonly_rows(&mut lines, &format!("Agent {}", index + 1), path);
    }
    lines
}

pub(super) fn render_plugin_skill_lines(capabilities: &[PluginCapability]) -> Vec<String> {
    let skills = capabilities
        .iter()
        .filter_map(|capability| match capability {
            PluginCapability::Skill { path } => Some(path.display().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut lines = vec![String::new(), "[skills]".to_owned()];
    if skills.is_empty() {
        lines.push(render_config_readonly_row("Skill count", "0"));
        return lines;
    }
    for (index, path) in skills.iter().enumerate() {
        push_wrapped_readonly_rows(&mut lines, &format!("Skill {}", index + 1), path);
    }
    lines
}

pub(super) fn render_plugin_hook_lines(capabilities: &[PluginCapability]) -> Vec<String> {
    let hooks = capabilities
        .iter()
        .filter_map(|capability| match capability {
            PluginCapability::Hook {
                hook_kind,
                declared_effect,
                ..
            } => Some((*hook_kind, *declared_effect)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut lines = vec![String::new(), "[hooks]".to_owned()];
    if hooks.is_empty() {
        lines.push(render_config_readonly_row("Hook count", "0"));
        return lines;
    }
    lines.push(render_config_readonly_row(
        "Hook count",
        &hooks.len().to_string(),
    ));
    lines.push(render_config_readonly_row(
        "Hook kinds",
        &plugin_hook_kind_summary(&hooks),
    ));
    lines.push(render_config_readonly_row(
        "Hook effects",
        &plugin_hook_effect_summary(&hooks),
    ));
    lines.push(render_config_readonly_row(
        "Runtime",
        "trusted hooks run through execution backend",
    ));
    lines.push(render_config_readonly_row(
        "Evidence",
        "mutating hooks record workspace evidence",
    ));
    lines.push(render_config_readonly_row(
        "Audit",
        "session records backend, profile, network",
    ));
    lines.push(render_config_readonly_row(
        "Inspect",
        "run /doctor for command and issue details",
    ));
    lines
}

pub(super) fn plugin_hook_kind_summary(
    hooks: &[(sigil_kernel::PluginHookKind, ToolEffect)],
) -> String {
    let mut context = 0;
    let mut compaction = 0;
    let mut verification = 0;
    let mut event = 0;
    for (kind, _) in hooks {
        match kind {
            sigil_kernel::PluginHookKind::Context => context += 1,
            sigil_kernel::PluginHookKind::Compaction => compaction += 1,
            sigil_kernel::PluginHookKind::Verification => verification += 1,
            sigil_kernel::PluginHookKind::Event => event += 1,
        }
    }
    format!("context={context} compaction={compaction} verification={verification} event={event}")
}

pub(super) fn plugin_hook_effect_summary(
    hooks: &[(sigil_kernel::PluginHookKind, ToolEffect)],
) -> String {
    let mut read_only = 0;
    let mut workspace_write = 0;
    let mut external_write = 0;
    let mut network = 0;
    let mut unknown = 0;
    for (_, effect) in hooks {
        match effect {
            ToolEffect::ReadOnly => read_only += 1,
            ToolEffect::WorkspaceWrite => workspace_write += 1,
            ToolEffect::ExternalWrite => external_write += 1,
            ToolEffect::Network => network += 1,
            ToolEffect::Unknown => unknown += 1,
        }
    }
    format!(
        "read_only={read_only} workspace_write={workspace_write} external_write={external_write} network={network} unknown={unknown}"
    )
}

pub(super) fn render_plugin_mcp_lines(capabilities: &[PluginCapability]) -> Vec<String> {
    let servers = capabilities
        .iter()
        .filter_map(|capability| match capability {
            PluginCapability::McpServer {
                name,
                command,
                args,
                startup,
                required,
                approval,
                egress_logging,
                allow_secrets,
            } => Some((
                name,
                command,
                args,
                startup,
                *required,
                approval,
                egress_logging,
                allow_secrets,
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut lines = vec![String::new(), "[mcp servers]".to_owned()];
    if servers.is_empty() {
        lines.push(render_config_readonly_row("MCP count", "0"));
        return lines;
    }
    for (
        index,
        (name, command, args, startup, required, approval, egress_logging, allow_secrets),
    ) in servers.iter().enumerate()
    {
        let label = format!("MCP {}", index + 1);
        lines.push(render_config_readonly_row(&label, name.as_str()));
        push_wrapped_readonly_rows(
            &mut lines,
            &format!("{label} command"),
            &command_with_args(command, args),
        );
        lines.push(render_config_readonly_row(
            &format!("{label} startup"),
            startup.as_str(),
        ));
        lines.push(render_config_readonly_row(
            &format!("{label} required"),
            bool_summary(*required),
        ));
        lines.push(render_config_readonly_row(
            &format!("{label} policy"),
            &plugin_capability_policy_summary(approval, **egress_logging, **allow_secrets),
        ));
    }
    lines
}

pub(super) fn plugin_capability_policy_summary(
    approval: &ApprovalMode,
    egress_logging: bool,
    allow_secrets: bool,
) -> String {
    format!(
        "approval={} egress={} secrets={}",
        approval.as_str(),
        bool_summary(egress_logging),
        secrets_summary(allow_secrets)
    )
}

pub(super) fn secrets_summary(allow_secrets: bool) -> &'static str {
    if allow_secrets { "allowed" } else { "blocked" }
}

pub(super) fn command_with_args(command: &str, args: &[String]) -> String {
    std::iter::once(command.to_owned())
        .chain(args.iter().map(|arg| command_arg_display(arg)))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn command_arg_display(arg: &str) -> String {
    if arg.chars().any(char::is_whitespace) {
        format!("{arg:?}")
    } else {
        arg.to_owned()
    }
}

pub(super) fn plugin_review_action_label(decision: PluginTrustDecision) -> &'static str {
    match decision {
        PluginTrustDecision::Trusted => "approved",
        PluginTrustDecision::Disabled => "denied",
        PluginTrustDecision::NeedsReview => "needs review",
    }
}
