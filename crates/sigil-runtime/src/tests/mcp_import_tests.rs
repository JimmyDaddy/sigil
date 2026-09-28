use super::*;

#[test]
fn mcp_import_selected_file_bounds_actual_bytes_and_refuses_non_regular_targets() -> Result<()> {
    let temp = tempfile::tempdir()?;
    assert!(preview_mcp_configuration_import_file(temp.path()).is_err());
    let path = temp.path().join("selected.json");
    std::fs::write(&path, vec![b' '; MAX_IMPORT_BYTES + 1])?;
    assert!(preview_mcp_configuration_import_file(&path).is_err());
    std::fs::write(
        &path,
        br#"{"mcpServers":{"selected":{"command":"python3"}}}"#,
    )?;
    assert_eq!(
        preview_mcp_configuration_import_file(&path)?
            .summaries()
            .len(),
        1
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn mcp_import_explicit_symlink_is_read_but_fifo_never_blocks() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let file = temp.path().join("source.json");
    std::fs::write(
        &file,
        br#"{"mcpServers":{"selected":{"command":"python3"}}}"#,
    )?;
    let link = temp.path().join("selected-link.json");
    std::os::unix::fs::symlink(&file, &link)?;
    assert_eq!(
        preview_mcp_configuration_import_file(&link)?.summaries()[0].name,
        "selected"
    );
    let fifo = temp.path().join("fifo");
    ensure!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()?
            .success(),
        "fixture FIFO creation failed"
    );
    assert!(preview_mcp_configuration_import_file(&fifo).is_err());
    Ok(())
}

#[test]
fn mcp_import_previews_typed_candidates_without_copying_credentials() -> Result<()> {
    let preview = preview_mcp_configuration_import(br#"{
        "mcpServers": {
            "local": {"command":"python3", "args":["./server.py"], "description":"Read records", "env":{"TOKEN":"super-private-value"}},
            "remote": {"url":"https://example.test/mcp", "type":"http", "headers":{"Authorization":"Bearer private-value"}}
        },
        "inputs":[{"password":"another-private-value"}]
    }"#)?;
    assert!(preview.root_fields_ignored());
    assert!(preview.summaries().iter().all(|summary| summary.importable));
    let displayed = serde_json::to_string(preview.summaries())?;
    assert!(!displayed.contains("private-value"));
    assert!(!displayed.contains("server.py"));
    let selected = preview.selected_configurations(&[0, 1], &[])?;
    let persisted = serde_json::to_string(&selected)?;
    assert!(!persisted.contains("private-value"));
    assert!(
        selected.iter().all(
            |server| !server.required && server.startup == sigil_kernel::McpServerStartup::Lazy
        )
    );
    assert!(
        selected
            .iter()
            .all(|server| server.trust.approval_default == sigil_kernel::ApprovalMode::Ask)
    );
    assert!(selected[0].stdio().expect("stdio candidate").2.is_empty());
    assert!(
        selected[1]
            .streamable_http()
            .expect("remote candidate")
            .http_headers
            .is_empty()
    );
    Ok(())
}

#[test]
fn mcp_import_isolates_invalid_entries_and_requires_exact_nonconflicting_selection() -> Result<()> {
    let preview = preview_mcp_configuration_import(
        br#"{"mcpServers": {
        "ambiguous":{"command":"python3","url":"https://example.test/mcp"},
        "bad":{"command":17},
        "sse":{"type":"sse","url":"https://example.test/sse"},
        "valid":{"command":"python3","args":[],"unknown":{"secret":"private"}}
    }}"#,
    )?;
    assert_eq!(
        preview
            .summaries()
            .iter()
            .filter(|item| item.importable)
            .count(),
        1
    );
    let selected = preview.selected_configurations(&[3], &[])?;
    assert_eq!(selected[0].name, "valid");
    assert!(
        preview.summaries()[3]
            .issues
            .contains(&McpImportIssue::IgnoredFields)
    );
    assert!(preview.selected_configurations(&[0], &[]).is_err());
    assert!(preview.selected_configurations(&[3, 3], &[]).is_err());
    assert!(preview.selected_configurations(&[3], &selected).is_err());
    assert!(preview.selected_configurations(&[99], &[]).is_err());
    Ok(())
}

#[test]
fn mcp_import_rejects_other_sigil_schema_and_bounds_untrusted_display() -> Result<()> {
    assert!(preview_mcp_configuration_import(br#"{"mcp_servers": []}"#).is_err());
    assert!(preview_mcp_configuration_import(&vec![b' '; MAX_IMPORT_BYTES + 1]).is_err());
    let document =
        json!({"mcpServers":{"records":{"command":"python3","description":"记录".repeat(1024)}}});
    let preview = preview_mcp_configuration_import(&serde_json::to_vec(&document)?)?;
    assert!(preview.summaries()[0].description.len() <= MAX_PREVIEW_TEXT_BYTES);
    assert!(!preview.summaries()[0].description.is_empty());
    assert_eq!(
        preview.selected_configurations(&[0], &[])?[0].description,
        "记录".repeat(1024)
    );
    Ok(())
}
