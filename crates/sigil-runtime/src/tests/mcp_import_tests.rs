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

fn core_import_config(mcp_document: &str) -> Result<RootConfig> {
    RootConfig::parse_persisted(&format!(
        r#"config_version = 2
{mcp_document}
[agent]
connection = "fixture"
model = "fixture-model"
[composition]
profile = "core"
enhancements = ["task_orchestration"]
[task]
enabled = true
routing_policy = "auto"
multi_agent_mode = "none"
[memory]
writable = ["deferred and unchanged"]
[web]
search_route = "deferred and unchanged"
"#
    ))
}

#[test]
fn mcp_import_selects_only_its_owner_and_preserves_empty_deferred_and_other_documents() -> Result<()>
{
    let current = core_import_config("mcp_servers = []")?;
    let original = current.persisted_toml()?;
    let import = preview_mcp_configuration_import(
        br#"{"mcpServers":{"selected":{"command":"must-not-run-during-import"}}}"#,
    )?;
    let next = import.selected_root_configuration(&[0], &current)?;
    assert_eq!(current.persisted_toml()?, original);
    assert_eq!(next.composition.profile, current.composition.profile);
    assert_eq!(
        next.composition.enhancements,
        BTreeSet::from([
            OptionalCapability::TaskOrchestration,
            OptionalCapability::Mcp
        ])
    );
    assert_eq!(next.task, current.task);
    let before: toml::Value = toml::from_str(&original)?;
    let saved: toml::Value = toml::from_str(&next.persisted_toml()?)?;
    for key in ["memory", "web"] {
        assert_eq!(saved.get(key), before.get(key), "{key}");
    }
    let effective = next.with_effective_composition()?;
    assert_eq!(effective.mcp_servers.len(), 1);
    assert_eq!(effective.mcp_servers[0].name, "selected");
    assert_eq!(
        effective.selected_capabilities(),
        next.composition.enhancements
    );
    assert_eq!(
        effective.mcp_servers[0].startup,
        sigil_kernel::McpServerStartup::Lazy
    );
    assert!(!effective.mcp_servers[0].required);
    assert_eq!(
        effective.mcp_servers[0].trust.approval_default,
        sigil_kernel::ApprovalMode::Ask
    );
    Ok(())
}

#[test]
fn mcp_import_reloads_existing_deferred_servers_before_conflict_detection() -> Result<()> {
    let current = core_import_config(
        r#"[[mcp_servers]]
name = "existing"
transport = "stdio"
command = "must-not-run-existing"
args = ["preserve-argument"]
description = "preserve existing declaration"
startup = "lazy"
required = false
"#,
    )?;
    assert!(
        current.mcp_servers.is_empty(),
        "unselected owner stays deferred"
    );
    let import = preview_mcp_configuration_import(
        br#"{"mcpServers":{"selected":{"command":"must-not-run-selected"}}}"#,
    )?;
    let next = import.selected_root_configuration(&[0], &current)?;
    assert_eq!(next.mcp_servers.len(), 2);
    assert_eq!(next.mcp_servers[0].name, "existing");
    assert_eq!(
        next.mcp_servers[0].description,
        "preserve existing declaration"
    );
    assert_eq!(
        next.mcp_servers[0].stdio().expect("stdio declaration").1,
        ["preserve-argument"]
    );
    assert_eq!(next.mcp_servers[1].name, "selected");
    let conflict = preview_mcp_configuration_import(
        br#"{"mcpServers":{"existing":{"command":"must-not-replace"}}}"#,
    )?;
    assert!(
        conflict
            .selected_root_configuration(&[0], &current)
            .is_err()
    );
    assert!(import.selected_root_configuration(&[], &current).is_err());
    assert!(
        import
            .selected_root_configuration(&[0, 0], &current)
            .is_err()
    );
    let duplicate_existing = core_import_config(
        "[[mcp_servers]]\nname = 'existing'\ntransport = 'stdio'\ncommand = 'one'\n\
         [[mcp_servers]]\nname = 'existing'\ntransport = 'stdio'\ncommand = 'two'",
    )?;
    assert!(
        import
            .selected_root_configuration(&[0], &duplicate_existing)
            .is_err()
    );
    let invalid = core_import_config(
        "[[mcp_servers]]\nname = 'invalid-dormant'\ntransport = 'private-existing-value'",
    )?;
    let error = import
        .selected_root_configuration(&[0], &invalid)
        .err()
        .ok_or_else(|| anyhow!("invalid deferred transport was accepted"))?;
    assert!(!error.to_string().contains("private-existing-value"));
    Ok(())
}

#[tokio::test]
async fn mcp_import_reload_makes_discovery_and_activation_available_without_starting_servers()
-> Result<()> {
    let fixture = tempfile::tempdir()?;
    let current = core_import_config("mcp_servers = []")?;
    let import = preview_mcp_configuration_import(
        br#"{"mcpServers":{"selected":{"command":"must-not-run-during-discovery"}}}"#,
    )?;
    let mut reloaded = RootConfig::parse_persisted(
        &import
            .selected_root_configuration(&[0], &current)?
            .persisted_toml()?,
    )?
    .with_effective_composition()?;
    reloaded.storage.state_root =
        sigil_kernel::StorageRoot::Path(fixture.path().join("state").display().to_string());
    reloaded.storage.cache_root =
        sigil_kernel::StorageRoot::Path(fixture.path().join("cache").display().to_string());
    let capabilities = crate::provider_capabilities_for_name("deepseek")
        .ok_or_else(|| anyhow!("missing fixture capabilities"))?;
    let registry =
        crate::build_tool_registry(&reloaded, &capabilities, fixture.path().to_path_buf()).await?;
    assert!(registry.spec_for("mcp_catalog").is_some());
    assert!(registry.spec_for("mcp_activate_server").is_some());
    assert!(
        registry
            .specs()
            .iter()
            .all(|spec| !spec.name.starts_with("mcp__"))
    );
    let result = registry
        .execute(
            sigil_kernel::ToolContext::new(fixture.path(), 5),
            sigil_kernel::ToolCall {
                id: "import-discovery".to_owned(),
                name: "mcp_catalog".to_owned(),
                args_json: json!({"operation":"list_servers"}).to_string(),
            },
        )
        .await?;
    let catalog: Value = serde_json::from_str(&result.content)?;
    assert_eq!(catalog["servers"][0]["server_name"], "selected");
    assert!(
        registry
            .specs()
            .iter()
            .all(|spec| !spec.name.starts_with("mcp__"))
    );
    Ok(())
}
