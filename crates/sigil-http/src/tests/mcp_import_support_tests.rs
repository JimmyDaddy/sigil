use super::*;

fn import_context() -> Result<(tempfile::TempDir, HttpSupportContext)> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("sigil.toml");
    fs::write(
        &path,
        r#"
config_version = 2
[agent]
connection = "local"
model = "test"
[connections.local]
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:11434/v1"
credential = { source = "none" }
"#,
    )?;
    let context = HttpSupportContext::new(
        &path,
        root.path(),
        SupportBuildInfo::new("test", "test", "test", "debug"),
    );
    Ok((root, context))
}

const IMPORT: &[u8] = br#"{
  "future": true,
  "mcpServers": {
    "alpha": {"command":"python3", "args":["-c", "print('not executed by import')"], "env":{"API_KEY":"do_not_copy"}},
    "beta": {"url":"https://example.test/mcp", "headers":{"Authorization":"Bearer do_not_copy"}}
  }
}"#;

#[test]
fn mcp_import_is_explicit_cas_bound_and_does_not_copy_credentials() -> Result<()> {
    let (_root, context) = import_context()?;
    let before = fs::read(&context.config_path)?;
    let preview = context.preview_mcp_import(IMPORT).expect("valid preview");
    assert!(preview.root_fields_ignored);
    assert_eq!(preview.candidates.len(), 2);
    assert_eq!(
        fs::read(&context.config_path)?,
        before,
        "preview performs no publication"
    );
    let serialized = serde_json::to_string(&preview)?;
    assert!(!serialized.contains("do_not_copy"));
    assert!(!serialized.contains("python3"));
    assert!(!serialized.contains("not executed"));
    let selection = HttpMcpImportApplyRequest {
        preview_id: preview.preview_id,
        selected_indices: vec![0],
    };
    let mut changed = before;
    changed.extend_from_slice(b"\n# another editor changed this configuration\n");
    fs::write(&context.config_path, &changed)?;
    assert!(matches!(
        context.apply_mcp_import(selection, configuration_capsule("test-import")),
        Err(HttpMcpImportFailure::Stale)
    ));
    assert_eq!(fs::read(&context.config_path)?, changed);
    let preview = context.preview_mcp_import(IMPORT).expect("fresh preview");
    let (result, _receipt) = context
        .apply_mcp_import(
            HttpMcpImportApplyRequest {
                preview_id: preview.preview_id,
                selected_indices: vec![0],
            },
            configuration_capsule("test-import-fresh"),
        )
        .expect("selected publish");
    assert_eq!(result.imported_names, ["alpha"]);
    let config = RootConfig::load_persisted(&context.config_path)?;
    assert_eq!(config.mcp_servers.len(), 1);
    assert_eq!(config.mcp_servers[0].name, "alpha");
    assert_eq!(
        config.mcp_servers[0].startup,
        sigil_kernel::McpServerStartup::Lazy
    );
    assert!(!fs::read_to_string(&context.config_path)?.contains("do_not_copy"));
    Ok(())
}

#[test]
fn mcp_import_invalid_selection_preserves_preview_for_retry() -> Result<()> {
    let (_root, context) = import_context()?;
    let preview = context.preview_mcp_import(IMPORT).expect("valid preview");
    let before = fs::read(&context.config_path)?;
    assert!(matches!(
        context.apply_mcp_import(
            HttpMcpImportApplyRequest {
                preview_id: preview.preview_id.clone(),
                selected_indices: vec![100],
            },
            configuration_capsule("test-invalid")
        ),
        Err(HttpMcpImportFailure::Invalid)
    ));
    assert_eq!(fs::read(&context.config_path)?, before);
    context
        .apply_mcp_import(
            HttpMcpImportApplyRequest {
                preview_id: preview.preview_id.clone(),
                selected_indices: vec![1],
            },
            configuration_capsule("test-valid"),
        )
        .expect("retry selects the retained preview");
    assert!(matches!(
        context.apply_mcp_import(
            HttpMcpImportApplyRequest {
                preview_id: preview.preview_id,
                selected_indices: vec![0],
            },
            configuration_capsule("test-used")
        ),
        Err(HttpMcpImportFailure::Stale)
    ));
    Ok(())
}
