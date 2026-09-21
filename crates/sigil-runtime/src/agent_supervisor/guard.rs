use sigil_kernel::ToolRegistryScope;

const UNGUARDED_WRITE_TOOL_NAMES: &[&str] = &[
    "write_file",
    "edit_file",
    "delete_file",
    "apply_changeset",
    "exec_command",
    "exec_input",
];
const WRITE_CAPABLE_TOOL_PREFIXES: &[&str] = &["mcp__"];

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
