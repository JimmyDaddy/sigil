use super::*;

#[test]
fn plan_review_deny_scope_uses_registered_web_tool_names() {
    let scope = plan_review_deny_scope();

    assert!(scope.allows("webfetch"));
    assert!(scope.allows("websearch"));
}

#[test]
fn read_only_role_scope_excludes_mutation_tools() {
    let scope = read_only_role_tool_scope();

    assert!(!scope.allow_all);
    assert!(scope.allows("read_file"));
    assert!(scope.allows("grep"));
    assert!(!scope.allows("write_file"));
    assert!(!scope.allows("terminal_start"));
}
