use super::*;

#[test]
fn from_root_config_initializes_mcp_statuses_from_startup_mode() {
    let mut config = test_config();
    config.mcp_servers.push(sigil_kernel::McpServerConfig {
        name: "eager".to_owned(),
        command: "mcp-eager".to_owned(),
        startup: McpServerStartup::Eager,
        ..Default::default()
    });
    config.mcp_servers.push(sigil_kernel::McpServerConfig {
        name: "lazy".to_owned(),
        command: "mcp-lazy".to_owned(),
        startup: McpServerStartup::Lazy,
        required: false,
        ..Default::default()
    });

    let app = AppState::from_root_config(Path::new("sigil.toml"), &config);

    assert_eq!(
        app.mcp_server_runtime_status_label("eager").as_deref(),
        Some("ready")
    );
    assert_eq!(
        app.mcp_server_runtime_status_label("lazy").as_deref(),
        Some("deferred")
    );
}

#[test]
fn code_intelligence_sidebar_sorts_diagnostics_and_collapses_overflow() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.code_intelligence_server_lines.insert(
        "rust-analyzer".to_owned(),
        "rust-analyzer: ready".to_owned(),
    );
    app.code_intelligence_diagnostics_line = Some("diagnostics: 8".to_owned());
    app.code_intelligence_diagnostics_by_path = std::collections::BTreeMap::from([
        (
            "src/a.rs".to_owned(),
            ApprovalDiagnosticSummary {
                errors: 1,
                warnings: 0,
            },
        ),
        (
            "src/b.rs".to_owned(),
            ApprovalDiagnosticSummary {
                errors: 3,
                warnings: 0,
            },
        ),
        (
            "src/c.rs".to_owned(),
            ApprovalDiagnosticSummary {
                errors: 3,
                warnings: 2,
            },
        ),
        (
            "src/d.rs".to_owned(),
            ApprovalDiagnosticSummary {
                errors: 1,
                warnings: 5,
            },
        ),
        (
            "src/e.rs".to_owned(),
            ApprovalDiagnosticSummary {
                errors: 0,
                warnings: 1,
            },
        ),
    ]);

    let lines = app.code_intelligence_sidebar_lines();
    let diagnostics_index = lines
        .iter()
        .position(|line| line == "latest diagnostics: 5 files")
        .expect("diagnostics header should be present");

    assert_eq!(
        lines.first().map(String::as_str),
        Some("rust-analyzer: ready")
    );
    assert_eq!(lines.get(1).map(String::as_str), Some("diagnostics: 8"));
    assert_eq!(
        lines.get(diagnostics_index + 1).map(String::as_str),
        Some("src/c.rs: 3 errors 2 warnings")
    );
    assert_eq!(
        lines.get(diagnostics_index + 2).map(String::as_str),
        Some("src/b.rs: 3 errors")
    );
    assert_eq!(
        lines.get(diagnostics_index + 3).map(String::as_str),
        Some("src/d.rs: 1 error 5 warnings")
    );
    assert_eq!(
        lines.get(diagnostics_index + 4).map(String::as_str),
        Some("src/a.rs: 1 error")
    );
    assert_eq!(lines.last().map(String::as_str), Some("+1 more files"));
}
