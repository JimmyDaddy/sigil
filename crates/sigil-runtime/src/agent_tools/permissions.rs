use super::*;

pub(super) fn tool_scope_summary(scope: &sigil_kernel::ToolRegistryScope) -> String {
    if scope.allow_all {
        return "all tools".to_owned();
    }
    let names = scope.names.iter().cloned().collect::<Vec<_>>().join(",");
    let prefixes = scope.prefixes.join(",");
    if names.is_empty() && prefixes.is_empty() {
        "no tools".to_owned()
    } else if prefixes.is_empty() {
        format!("names={names}")
    } else if names.is_empty() {
        format!("prefixes={prefixes}")
    } else {
        format!("names={names}; prefixes={prefixes}")
    }
}

pub(super) fn tool_scope_is_safe_readonly_for_auto_spawn(
    scope: &sigil_kernel::ToolRegistryScope,
) -> bool {
    const SAFE_READONLY_TOOLS: &[&str] = &[
        "read_file",
        "ls",
        "glob",
        "grep",
        "code_symbols",
        "code_workspace_symbols",
        "code_definition",
        "code_references",
        "code_diagnostics",
        crate::LOAD_SKILL_TOOL_NAME,
    ];
    !scope.allow_all
        && scope.prefixes.is_empty()
        && scope
            .names
            .iter()
            .all(|name| SAFE_READONLY_TOOLS.contains(&name.as_str()))
}

pub(super) fn effective_child_permission_config(
    parent: &PermissionConfig,
    role: &PermissionConfig,
    profile: &PermissionConfig,
) -> PermissionConfig {
    let parent_role = apply_child_permission_overlay(parent.clone(), role);
    apply_child_permission_overlay(parent_role, profile)
}

pub(super) fn apply_child_permission_overlay(
    mut base: PermissionConfig,
    overlay: &PermissionConfig,
) -> PermissionConfig {
    if overlay.mode != PermissionConfig::default().mode {
        base.mode = strictest_permission_mode(base.mode, overlay.mode);
    }

    for (tool_name, mode) in &overlay.tools {
        base.tools.insert(
            tool_name.clone(),
            strictest_mode(cap_mode_for_tool(&base, tool_name), *mode),
        );
    }

    let capped_rules = overlay
        .rules
        .iter()
        .map(|rule| cap_permission_rule(&base, rule))
        .collect::<Vec<_>>();
    base.rules.extend(capped_rules);

    apply_external_directory_overlay(&mut base, overlay);
    base
}

pub(super) fn access_mode(config: &PermissionConfig, access: ToolAccess) -> ApprovalMode {
    config.mode.baseline_for_access(access)
}

pub(super) fn cap_mode_for_tool(config: &PermissionConfig, tool_name: &str) -> ApprovalMode {
    config
        .tools
        .get(tool_name)
        .copied()
        .unwrap_or_else(|| access_mode(config, guessed_tool_access(tool_name)))
}

pub(super) fn cap_permission_rule(
    cap: &PermissionConfig,
    rule: &sigil_kernel::PermissionRule,
) -> sigil_kernel::PermissionRule {
    let cap_mode = rule
        .tool_name
        .as_deref()
        .map(|tool_name| cap_mode_for_tool(cap, tool_name))
        .unwrap_or_else(|| cap.mode.baseline_for_access(ToolAccess::Execute));
    let mut capped = rule.clone();
    capped.mode = strictest_mode(cap_mode, rule.mode);
    capped
}

pub(super) fn cap_external_directory_rule(
    cap: &PermissionConfig,
    rule: &sigil_kernel::ExternalDirectoryRule,
) -> sigil_kernel::ExternalDirectoryRule {
    let cap_mode = if cap.external_directory.enabled {
        cap.external_directory.default_mode
    } else {
        ApprovalMode::Deny
    };
    let mut capped = rule.clone();
    capped.mode = strictest_mode(cap_mode, rule.mode);
    capped
}

pub(super) fn apply_external_directory_overlay(
    base: &mut PermissionConfig,
    overlay: &PermissionConfig,
) {
    let default_external = sigil_kernel::ExternalDirectoryConfig::default();
    if overlay.external_directory.enabled {
        base.external_directory.enabled &= overlay.external_directory.enabled;
    }
    if overlay.external_directory.default_mode != default_external.default_mode {
        base.external_directory.default_mode = strictest_mode(
            base.external_directory.default_mode,
            overlay.external_directory.default_mode,
        );
    }
    let capped_rules = overlay
        .external_directory
        .rules
        .iter()
        .map(|rule| cap_external_directory_rule(base, rule))
        .collect::<Vec<_>>();
    base.external_directory.rules.extend(capped_rules);
}

pub(super) fn guessed_tool_access(tool_name: &str) -> ToolAccess {
    match tool_name {
        "read_file"
        | "ls"
        | "glob"
        | "grep"
        | "terminal_read"
        | LIST_AGENTS_TOOL_NAME
        | "read_agent_result"
        | "code_symbols"
        | "code_workspace_symbols"
        | "code_definition"
        | "code_references"
        | "code_diagnostics" => ToolAccess::Read,
        "write_file" | "edit_file" | "delete_file" | "apply_changeset" => ToolAccess::Write,
        "bash"
        | "terminal_start"
        | "terminal_input"
        | "terminal_resize"
        | "terminal_cancel"
        | SPAWN_AGENT_TOOL_NAME
        | WAIT_AGENT_TOOL_NAME
        | CANCEL_AGENT_TOOL_NAME
        | MESSAGE_AGENT_TOOL_NAME
        | CLOSE_AGENT_TOOL_NAME
        | crate::LOAD_SKILL_TOOL_NAME => ToolAccess::Execute,
        name if name.starts_with("mcp__") => ToolAccess::Network,
        _ => ToolAccess::Execute,
    }
}

pub(super) fn strictest_mode(left: ApprovalMode, right: ApprovalMode) -> ApprovalMode {
    match (left, right) {
        (ApprovalMode::Deny, _) | (_, ApprovalMode::Deny) => ApprovalMode::Deny,
        (ApprovalMode::Ask, _) | (_, ApprovalMode::Ask) => ApprovalMode::Ask,
        (ApprovalMode::Allow, ApprovalMode::Allow) => ApprovalMode::Allow,
    }
}

pub(super) fn strictest_permission_mode(
    left: PermissionMode,
    right: PermissionMode,
) -> PermissionMode {
    left.min(right)
}
