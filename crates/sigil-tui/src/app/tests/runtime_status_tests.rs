use super::{McpServerRuntimeStatus, summarize_mcp_failure};

#[test]
fn mcp_failure_summary_removes_redundant_server_prefixes() {
    assert_eq!(
        summarize_mcp_failure("connection refused", None),
        "connection refused"
    );
    assert_eq!(
        summarize_mcp_failure(
            "MCP server filesystem initialize timed out",
            Some("filesystem")
        ),
        "initialize timed out"
    );
    assert_eq!(
        summarize_mcp_failure(
            "startup failed: MCP server filesystem deadline elapsed",
            Some("filesystem"),
        ),
        "startup failed: MCP server deadline elapsed"
    );
    assert_eq!(
        McpServerRuntimeStatus::Failed {
            message: "MCP server filesystem initialize timed out".to_owned(),
        }
        .label_for_server(Some("filesystem")),
        "failed: initialize timed out"
    );
}
