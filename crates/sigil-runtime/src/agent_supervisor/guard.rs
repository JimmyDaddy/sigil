use sigil_kernel::ToolRegistryScope;

#[cfg(test)]
const WRITE_CAPABLE_TOOL_NAMES: &[&str] = &["write_file", "edit_file", "apply_changeset", "bash"];
const UNGUARDED_WRITE_TOOL_NAMES: &[&str] = &[
    "write_file",
    "edit_file",
    "delete_file",
    "apply_changeset",
    "bash",
];
const WRITE_CAPABLE_TOOL_PREFIXES: &[&str] = &["mcp__"];

#[cfg(test)]
pub(super) fn tool_scope_is_write_capable(scope: &ToolRegistryScope) -> bool {
    scope.allow_all
        || WRITE_CAPABLE_TOOL_NAMES
            .iter()
            .any(|tool_name| scope.allows(tool_name))
        || scope.prefixes.iter().any(|prefix| {
            WRITE_CAPABLE_TOOL_PREFIXES
                .iter()
                .any(|write_prefix| prefix.starts_with(write_prefix))
        })
}

pub(super) fn tool_scope_has_unguarded_write_capability(scope: &ToolRegistryScope) -> bool {
    scope.allow_all
        || UNGUARDED_WRITE_TOOL_NAMES
            .iter()
            .any(|tool_name| scope.allows(tool_name))
        || scope.prefixes.iter().any(|prefix| {
            WRITE_CAPABLE_TOOL_PREFIXES
                .iter()
                .any(|write_prefix| prefix.starts_with(write_prefix))
        })
}
