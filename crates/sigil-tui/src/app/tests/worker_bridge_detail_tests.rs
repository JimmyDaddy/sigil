use super::*;
use serde_json::json;
use sigil_kernel::ToolResultMeta;

#[test]
fn mcp_activation_event_detail_formats_scope_and_errors() {
    assert_eq!(
        mcp_activation_event_detail(Some("filesystem"), &McpActivationStatus::Deferred),
        "server=filesystem deferred"
    );
    assert_eq!(
        mcp_activation_event_detail(None, &McpActivationStatus::Ready { added_tools: 3 }),
        "ready tools=3"
    );
    assert_eq!(
        mcp_activation_event_detail(
            Some("filesystem"),
            &McpActivationStatus::Failed {
                error: "request failed\n\nCaused by:\n  0: bad response".to_owned(),
            }
        ),
        "server=filesystem failed bad response"
    );
}

#[test]
fn agent_tool_name_matches_all_agent_tool_surfaces() {
    for name in [
        "spawn_agent",
        "wait_agent",
        "read_agent_result",
        "message_agent",
        "close_agent",
    ] {
        assert!(agent_tool_name(name));
    }
    assert!(!agent_tool_name("read_file"));
}

#[test]
fn code_intelligence_server_lines_format_languages_and_statuses() {
    let result = ToolResult::ok(
        "call-1",
        "code_status",
        "{}",
        ToolResultMeta {
            details: json!({
                "code_intelligence": {
                    "servers": [
                        { "server": "rust-analyzer", "status": "ready", "languages": ["rust"] },
                        {
                            "server": "tsserver",
                            "status": "configured",
                            "languages": ["typescript", "javascript"]
                        },
                        { "server": "custom", "status": "degraded" }
                    ]
                }
            }),
            ..ToolResultMeta::default()
        },
    );

    let lines = code_intelligence_server_lines(&result).expect("server lines should be present");
    assert_eq!(
        lines[0],
        (
            "rust-analyzer".to_owned(),
            "rust: ready rust-analyzer".to_owned()
        )
    );
    assert_eq!(
        lines[1],
        (
            "tsserver".to_owned(),
            "typescript/javascript: configured tsserver".to_owned(),
        )
    );
    assert_eq!(
        lines[2],
        ("custom".to_owned(), "custom: degraded".to_owned())
    );
}

#[test]
fn code_diagnostics_helpers_cover_clean_results_and_paths() {
    let clean = ToolResult::ok(
        "call-clean",
        "code_diagnostics",
        json!({ "results": [] }).to_string(),
        ToolResultMeta::default(),
    );
    assert_eq!(
        code_diagnostics_status_line(&clean),
        Some("diagnostics clean".to_owned())
    );

    let result = ToolResult::ok(
        "call-2",
        "code_diagnostics",
        json!({
            "query": { "paths": [".\\src\\main.rs", "src/lib.rs"] },
            "results": [
                { "path": ".\\src\\main.rs", "severity": "error" },
                { "path": "src/main.rs", "severity": "warning" },
                { "path": "ignored.rs", "severity": "info" }
            ]
        })
        .to_string(),
        ToolResultMeta::default(),
    );

    assert_eq!(
        code_diagnostics_status_line(&result),
        Some("diagnostics 1 errors 1 warnings".to_owned())
    );
    let summaries = code_diagnostics_by_path(&result).expect("diagnostics should parse");
    assert_eq!(
        summaries.get("src/main.rs"),
        Some(&ApprovalDiagnosticSummary {
            errors: 1,
            warnings: 1,
        })
    );
    assert_eq!(
        summaries.get("src/lib.rs"),
        Some(&ApprovalDiagnosticSummary::default())
    );
    assert_eq!(normalize_diagnostic_path(".\\src\\main.rs"), "src/main.rs");
    assert_eq!(
        code_diagnostics_sidebar_line("diagnostics clean"),
        "diagnostics: clean"
    );
    assert_eq!(
        code_diagnostics_sidebar_line("diagnostics 1 errors 2 warnings"),
        "diagnostics: 1 errors 2 warnings"
    );
    assert_eq!(
        code_diagnostics_sidebar_line("degraded tool error"),
        "diagnostics: degraded tool error"
    );
}
