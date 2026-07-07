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
    let _ = role;
    let mut effective = profile.clone();
    apply_child_permission_hard_caps(&mut effective, parent);
    effective
}

fn apply_child_permission_hard_caps(effective: &mut PermissionConfig, parent: &PermissionConfig) {
    if parent.mode == PermissionMode::ReadOnly {
        effective.mode = PermissionMode::ReadOnly;
    }
}
